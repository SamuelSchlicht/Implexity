// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::HyperDual;
use implexity_core::CaeError;
use implexity_linalg::sparse::CsrMatrix;

use super::{Scheme, SoftHistory};
use crate::util::contract;

#[derive(Debug, Clone)]
pub struct StateJacobians {
    pub residual: Vec<f64>,
    pub current: CsrMatrix,
    pub previous: CsrMatrix,
    pub params: CsrMatrix,
    pub extra: CsrMatrix,
    pub dt: Vec<f64>,
}

#[derive(Default)]
struct Blocks {
    current: Vec<(usize, usize, f64)>,
    previous: Vec<(usize, usize, f64)>,
    params: Vec<(usize, usize, f64)>,
    extra: Vec<(usize, usize, f64)>,
}

pub(super) struct Rule {
    pub(super) s: Vec<f64>,
    pub(super) w: Vec<f64>,
    pub(super) avf: bool,
    pub(super) wu: f64,
}

fn csr(n: usize, m: usize, t: &[(usize, usize, f64)]) -> Result<CsrMatrix, CaeError> {
    let rows: Vec<usize> = t.iter().map(|e| e.0).collect();
    let cols: Vec<usize> = t.iter().map(|e| e.1).collect();
    let vals: Vec<f64> = t.iter().map(|e| e.2).collect();
    CsrMatrix::from_triplets(n, m, &rows, &cols, &vals)
        .map_err(|e| CaeError::contract(format!("state Jacobian: {e}")))
}

struct ElementBlocks {
    cur: Vec<f64>,
    prev: Vec<f64>,
    design: [[f64; 2]; 16],
    history: [[f64; 6]; 12],
    dt: [f64; 16],
    siso: Option<[[f64; 14]; 6]>,
}

impl SoftHistory<'_> {
    pub(super) fn state_rule(&self) -> Result<Rule, CaeError> {
        Ok(match self.scheme {
            Scheme::AvfMidpoint { gauss_points } => {
                let (s, w) = super::avf::gauss_legendre(gauss_points)?;
                Rule { s, w, avf: true, wu: 1.0 }
            }
            _ => Rule { s: vec![1.0], w: vec![1.0], avf: false, wu: 1.0 - self.scheme.coeffs().af },
        })
    }

    pub(super) fn check_states(
        &self,
        t: usize,
        prev: &[f64],
        cur: &[f64],
        extra: Option<&[f64]>,
    ) -> Result<(), CaeError> {
        let l = self.layout;
        if t >= self.loading.steps() || prev.len() != l.len() || cur.len() != l.len() {
            return contract("step index or state length out of range");
        }
        if extra.is_some_and(|e| e.len() != l.n3) {
            return contract("the extra nodal load must be node-by-XYZ");
        }
        Ok(())
    }

    fn state_kinematics(&self, t: usize, prev: &[f64], u1: &[f64]) -> (Vec<f64>, Vec<f64>) {
        if matches!(self.scheme, Scheme::AvfMidpoint { .. }) {
            self.avf_kinematics(t, prev, u1)
        } else {
            self.newmark(t, prev, u1)
        }
    }


    pub fn state_residual(
        &self,
        t: usize,
        prev: &[f64],
        cur: &[f64],
        extra: Option<&[f64]>,
    ) -> Result<Vec<f64>, CaeError> {
        self.check_states(t, prev, cur, extra)?;
        let m = self.model;
        let l = self.layout;
        let c = self.scheme.coeffs();
        let dt = self.dt(t);
        let (u1, p1) = (&cur[..l.n3], &cur[l.p()..l.p() + l.np]);
        let (u0, v0, a0) = (&prev[..l.n3], &prev[l.v()..l.v() + l.n3], &prev[l.a()..l.a() + l.n3]);
        let rule = self.state_rule()?;
        let (r_out, rp) = if let Scheme::AvfMidpoint { gauss_points } = self.scheme {
            let pass = super::avf::Pass { jacobian: false, pull: None };
            let pf = self.avf_assemble(t, prev, u1, p1, extra, gauss_points, &pass)?;
            (pf.r, pf.rp)
        } else {
            let (r, rp, _, _) = self.forces(t, prev, u1, p1, extra, false)?;
            (r, rp)
        };
        let (vk, ak) = self.state_kinematics(t, prev, u1);
        let mut res = vec![0.0; l.len()];
        let mv = |v: &[f64]| self.ctx.m.matvec(v).map_err(|e| CaeError::contract(e.to_string()));
        let force: Vec<f64> = if rule.avf {
            let z = self.avf_inertia(t, prev, u1);
            let du: Vec<f64> = (0..l.n3).map(|i| (u1[i] - u0[i]) / dt).collect();
            let (mz, cd) = (mv(&z)?, self.damping_matvec(&du)?);
            (0..l.n3).map(|i| mz[i] + cd[i] + r_out[i]).collect()
        } else {
            let mut f: Vec<f64> = (0..l.n3).map(|i| rule.wu * r_out[i] + c.af * prev[l.r() + i]).collect();
            if c.inertia {
                let am_mix: Vec<f64> = (0..l.n3).map(|i| (1.0 - c.am) * ak[i] + c.am * a0[i]).collect();
                let v_mix: Vec<f64> = (0..l.n3).map(|i| (1.0 - c.af) * vk[i] + c.af * v0[i]).collect();
                let (ma, cv) = (mv(&am_mix)?, self.damping_matvec(&v_mix)?);
                for i in 0..l.n3 {
                    f[i] += ma[i] + cv[i];
                }
            }
            f
        };
        for i in 0..l.n3 {
            res[i] = if m.fixed[i] { u1[i] - self.prescribed_displacement(t, i) } else { force[i] };
            res[l.v() + i] = cur[l.v() + i] - vk[i];
            res[l.a() + i] = cur[l.a() + i] - ak[i];
            res[l.r() + i] = cur[l.r() + i] - r_out[i];
        }
        for a in 0..l.np {
            res[l.p() + a] = rule.wu * rp[a];
        }
        if l.ne_visc > 0 {
            for e in 0..m.ne() {
                let (factors, _) = self.branch_factors(e, dt);
                let s1 = &cur[l.s() + 6 * e..l.s() + 6 * e + 6];
                let siso = if factors.is_empty() {
                    [0.0; 6]
                } else {
                    let d = m.gather(e, u1, p1);
                    let ul: [[f64; 3]; 4] = core::array::from_fn(|a| core::array::from_fn(|i| d[3 * a + i]));
                    m.isochoric_stress(e, &ul, self.ctx.rho[e], self.ctx.theta[e])
                };
                for k in 0..6 {
                    res[l.s() + 6 * e + k] = s1[k] - siso[k];
                }
                for i in 0..l.nb {
                    let off = l.q() + 6 * (e * l.nb + i);
                    for k in 0..6 {
                        res[off + k] = cur[off + k]
                            - factors.get(i).map_or(0.0, |(ei, bi)| {
                                ei * prev[off + k] + bi * (s1[k] - prev[l.s() + 6 * e + k])
                            });
                    }
                }
            }
        }
        Ok(res)
    }

    fn siso_jacobian(&self, e: usize, d: &[f64; 16]) -> [[f64; 14]; 6] {
        let m = self.model;
        let mut out = [[0.0; 14]; 6];
        for (k, row) in out.iter_mut().enumerate() {
            let mut w = [0.0; 6];
            w[k] = 1.0;
            for (j, slot) in row.iter_mut().enumerate() {
                let seed = |i: usize| if i == j { 1.0 } else { 0.0 };
                let ul: [[HyperDual; 3]; 4] = core::array::from_fn(|a| {
                    core::array::from_fn(|i| HyperDual::new(d[3 * a + i], 0.0, seed(3 * a + i), 0.0))
                });
                let rho = HyperDual::new(self.ctx.rho[e], 0.0, seed(12), 0.0);
                let th = HyperDual::new(self.ctx.theta[e], 0.0, seed(13), 0.0);
                let tt = HyperDual::new(0.0, 1.0, 0.0, 0.0);
                let v = m.isochoric_stress_dot(e, &ul, rho, th, &w, tt).e12;
                *slot = if k < 3 { v } else { 0.5 * v };
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn state_element(
        &self,
        e: usize,
        rule: &Rule,
        prev: &[f64],
        u0: &[f64],
        p0: &[f64],
        u1: &[f64],
        p1: &[f64],
        dt: f64,
        current_only: bool,
    ) -> Result<ElementBlocks, CaeError> {
        let m = self.model;
        let l = self.layout;
        let nl = m.nl();
        let d0 = m.gather(e, u0, p0);
        let d1 = m.gather(e, u1, p1);
        let h = self.history_h(prev, e, dt);
        let (factors, gf) = self.branch_factors(e, dt);
        let mut out = ElementBlocks {
            cur: vec![0.0; 256],
            prev: vec![0.0; if rule.avf { 256 } else { 0 }],
            design: [[0.0; 2]; 16],
            history: [[0.0; 6]; 12],
            dt: [0.0; 16],
            siso: None,
        };
        let visc = l.ne_visc > 0 && !factors.is_empty();
        let (dh, dg) = if visc && !current_only {
            let taus = &self.model.materials[self.model.element_material[e]]
                .prony
                .as_ref()
                .map(|p| p.tau.clone())
                .unwrap_or_default();
            let mut dh = [0.0; 6];
            let mut dg = 0.0;
            for (i, ((ei, bi), tau)) in factors.iter().zip(taus).enumerate() {
                let (dei, dbi) = (-ei / tau, -0.5 * bi / tau);
                dg += dbi;
                let q = &prev[l.q() + 6 * (e * l.nb + i)..l.q() + 6 * (e * l.nb + i) + 6];
                let s = &prev[l.s() + 6 * e..l.s() + 6 * e + 6];
                for k in 0..6 {
                    dh[k] += dei * q[k] - dbi * s[k];
                }
            }
            (dh, dg)
        } else {
            ([0.0; 6], 0.0)
        };
        for (sg, wg) in rule.s.iter().zip(&rule.w) {
            let d: [f64; 16] =
                if rule.avf { core::array::from_fn(|k| d0[k] + sg * (d1[k] - d0[k])) } else { d1 };
            let local = m.element_local_full(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, gf)?;
            for a in 0..nl {
                for b in 0..nl {
                    out.cur[a * 16 + b] += wg * sg * local.hessian[a * 16 + b];
                    if rule.avf {
                        out.prev[a * 16 + b] += wg * (1.0 - sg) * local.hessian[a * 16 + b];
                    }
                }
                out.design[a][0] += wg * local.design[a][0];
                out.design[a][1] += wg * local.design[a][1];
            }
            if visc && !current_only {
                let hd = m.element_history_derivative(e, &d, self.ctx.rho[e]);
                let g1 = m.element_local(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, 1.0, false)?;
                let g0 = m.element_local(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, 0.0, false)?;
                for a in 0..nl {
                    let mut v = dg * (g1.gradient[a] - g0.gradient[a]);
                    if a < 12 {
                        for k in 0..6 {
                            out.history[a][k] += wg * hd[a][k];
                            v += hd[a][k] * dh[k];
                        }
                    }
                    out.dt[a] += wg * v;
                }
            }
        }
        if visc {
            out.siso = Some(self.siso_jacobian(e, &d1));
        }
        Ok(out)
    }


    #[allow(clippy::too_many_lines)]
    pub fn state_jacobians(
        &self,
        t: usize,
        prev: &[f64],
        cur: &[f64],
        extra: Option<&[f64]>,
    ) -> Result<StateJacobians, CaeError> {
        self.state_jacobians_internal(t, prev, cur, extra, false)
    }


    pub fn state_current_jacobian(
        &self,
        t: usize,
        prev: &[f64],
        cur: &[f64],
        extra: Option<&[f64]>,
    ) -> Result<CsrMatrix, CaeError> {
        Ok(self.state_jacobians_internal(t, prev, cur, extra, true)?.current)
    }

    fn state_flux_entries(&self) -> Result<Vec<(usize, usize, f64)>, CaeError> {
        let l = self.layout;
        let rule = self.state_rule()?;
        let mut entries = Vec::with_capacity(2 * l.n3);
        for i in 0..l.n3 {
            if !self.model.fixed[i] { entries.push((i, i, -rule.wu)); }
            entries.push((l.r() + i, i, 1.0));
        }
        Ok(entries)
    }

    pub fn state_flux_jacobian(&self) -> Result<CsrMatrix, CaeError> {
        csr(self.layout.len(), self.layout.n3, &self.state_flux_entries()?)
    }

    #[allow(clippy::too_many_lines)]
    fn state_jacobians_internal(
        &self,
        t: usize,
        prev: &[f64],
        cur: &[f64],
        extra: Option<&[f64]>,
        current_only: bool,
    ) -> Result<StateJacobians, CaeError> {
        self.check_states(t, prev, cur, extra)?;
        let residual = if current_only { Vec::new() } else { self.state_residual(t, prev, cur, extra)? };
        let m = self.model;
        let l = self.layout;
        let ne = m.ne();
        let nl = m.nl();
        let n = l.len();
        let c = self.scheme.coeffs();
        let dt = self.dt(t);
        let rule = self.state_rule()?;
        let (u1, p1) = (&cur[..l.n3], &cur[l.p()..l.p() + l.np]);
        let (u0, p0) = (&prev[..l.n3], &prev[l.p()..l.p() + l.np]);
        let (v0, a0) = (&prev[l.v()..l.v() + l.n3], &prev[l.a()..l.a() + l.n3]);
        let mut b = Blocks::default();
        let mut dtv = vec![0.0; n];
        let free = |i: usize| !m.fixed[i];

        let state_of = |dofs: &[usize; 16], k: usize| -> usize {
            if k < 12 { l.u() + dofs[k] } else { l.p() + dofs[k] - l.n3 }
        };

        let mut elements: Vec<(usize, ElementBlocks)> = Vec::with_capacity(ne);
        m.for_elements(
            |e| self.state_element(e, &rule, prev, u0, p0, u1, p1, dt, current_only),
            |e, blk| {
                elements.push((e, blk));
                Ok(())
            },
        )?;
        for (e, blk) in &elements {
            let e = *e;
            let dofs = m.element_dofs(e);
            for a in 0..nl {

                let targets: [(Option<usize>, f64); 2] = if a < 12 {
                    let i = dofs[a];
                    [(free(i).then_some(i), rule.wu), (Some(l.r() + i), -1.0)]
                } else {
                    [(Some(l.p() + dofs[a] - l.n3), rule.wu), (None, 0.0)]
                };
                for (row, wt) in targets {
                    let Some(row) = row else { continue };
                    for bb in 0..nl {
                        let col = state_of(&dofs, bb);
                        let v = blk.cur[a * 16 + bb];
                        if v != 0.0 {
                            b.current.push((row, col, wt * v));
                        }
                        if rule.avf {
                            let v = blk.prev[a * 16 + bb];
                            if v != 0.0 {
                                b.previous.push((row, col, wt * v));
                            }
                        }
                    }
                    b.params.push((row, e, wt * blk.design[a][0]));
                    b.params.push((row, ne + e, wt * blk.design[a][1]));
                    dtv[row] += wt * blk.dt[a];
                    if a < 12 && l.ne_visc > 0 {
                        let (factors, _) = self.branch_factors(e, dt);
                        let sum_b: f64 = factors.iter().map(|(_, bi)| bi).sum();
                        for k in 0..6 {
                            let hv = wt * blk.history[a][k];
                            if hv == 0.0 {
                                continue;
                            }
                            for (i, (ei, _)) in factors.iter().enumerate() {
                                b.previous.push((row, l.q() + 6 * (e * l.nb + i) + k, hv * ei));
                            }
                            b.previous.push((row, l.s() + 6 * e + k, -hv * sum_b));
                        }
                    }
                }
            }
        }

        let path = |sg: f64| -> Vec<f64> {
            if rule.avf { u0.iter().zip(u1).map(|(a, bb)| a + sg * (bb - a)).collect() } else { u1.to_vec() }
        };
        let rows_of = |i: usize| -> [(Option<usize>, f64); 2] {
            [(free(i).then_some(i), rule.wu), (Some(l.r() + i), -1.0)]
        };
        for pot in &m.potentials {
            for (sg, wg) in rule.s.iter().zip(&rule.w) {
                let ug = path(*sg);
                for (r, col, v) in pot.tangent(&ug, &self.params)? {
                    for (row, wt) in rows_of(r) {
                        let Some(row) = row else { continue };
                        b.current.push((row, l.u() + col, wt * wg * sg * v));
                        if rule.avf {
                            b.previous.push((row, l.u() + col, wt * wg * (1.0 - sg) * v));
                        }
                    }
                }
                for (r, k, v) in if current_only { Vec::new() } else { pot.force_params_jacobian(&ug, &self.params)? } {
                    if k >= 2 * ne {
                        return contract("nodal potential parameter Jacobian references unknown parameters");
                    }
                    for (row, wt) in rows_of(r) {
                        let Some(row) = row else { continue };
                        b.params.push((row, k, wt * wg * v));
                    }
                }
            }
        }
        let pressure = if rule.avf { self.avf_pressure(t) } else { self.pressure(t) };
        if pressure != 0.0 {
            for (f, face) in m.faces.iter().enumerate() {
                for (sg, wg) in rule.s.iter().zip(&rule.w) {
                    let ug = path(*sg);
                    let (_, jac) = m.face_local(f, &ug, pressure);
                    for node in face {
                        for i in 0..3 {
                            for (row, wt) in rows_of(3 * node + i) {
                                let Some(row) = row else { continue };
                                for k in 0..9 {
                                    let col = l.u() + 3 * face[k / 3] + k % 3;
                                    b.current.push((row, col, wt * wg * sg * jac[i][k]));
                                    if rule.avf {
                                        b.previous.push((row, col, wt * wg * (1.0 - sg) * jac[i][k]));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if !current_only { b.extra = self.state_flux_entries()?; }


        let (am, bk) = self.rayleigh;
        let mrow = |i: usize| self.ctx.m.row(i);
        let krow = |i: usize| self.ctx.kref.row(i);
        let presc = self.prescribes_support_rates();
        let data_col = |j: usize| presc && m.fixed[j];

        let push_mc = |list: &mut Vec<(usize, usize, f64)>,
                       row: usize,
                       i: usize,
                       col0: usize,
                       coeffs: (f64, f64),
                       on_data: (f64, f64)| {
            let pick = |j: usize| if data_col(j) { on_data } else { coeffs };
            let (mc, mv) = mrow(i);
            for (j, v) in mc.iter().zip(mv) {
                let (cm, cc) = pick(*j);
                list.push((row, col0 + j, (cm + cc * am) * v));
            }
            if bk != 0.0 {
                let (kc, kv) = krow(i);
                for (j, v) in kc.iter().zip(kv) {
                    let (_, cc) = pick(*j);
                    if cc != 0.0 {
                        list.push((row, col0 + j, cc * bk * v));
                    }
                }
            }
        };
        let (vk, ak) = self.state_kinematics(t, prev, u1);
        let (acc_term, vel_term): (Vec<f64>, Vec<f64>);
        if rule.avf {
            let (g_rate, _) = self.prescribed_rates(t);
            let (cm, cc) = (2.0 / (dt * dt), 1.0 / dt);
            for i in (0..l.n3).filter(|i| free(*i)) {
                push_mc(&mut b.current, i, i, l.u(), (cm, cc), (0.0, cc));
                push_mc(&mut b.previous, i, i, l.u(), (-cm, -cc), (0.0, -cc));
                push_mc(&mut b.previous, i, i, l.v(), (-2.0 / dt, 0.0), (-1.0 / dt, 0.0));
            }
            for i in 0..l.n3 {
                b.current.push((l.v() + i, l.v() + i, 1.0));
                b.current.push((l.a() + i, l.a() + i, 1.0));
                if !free(i) {

                    dtv[l.v() + i] += g_rate[i] / dt;
                    b.previous.push((l.a() + i, l.v() + i, 1.0 / dt));
                    dtv[l.a() + i] += (2.0 * g_rate[i] - v0[i]) / (dt * dt);
                    continue;
                }
                let du = u1[i] - u0[i];
                b.current.push((l.v() + i, l.u() + i, -2.0 / dt));
                b.previous.push((l.v() + i, l.u() + i, 2.0 / dt));
                b.previous.push((l.v() + i, l.v() + i, 1.0));
                dtv[l.v() + i] += 2.0 * du / (dt * dt);
                b.current.push((l.a() + i, l.u() + i, -2.0 / (dt * dt)));
                b.previous.push((l.a() + i, l.u() + i, 2.0 / (dt * dt)));
                b.previous.push((l.a() + i, l.v() + i, 2.0 / dt));
                dtv[l.a() + i] += 4.0 * du / (dt * dt * dt) - 2.0 * v0[i] / (dt * dt);
            }

            let zm: Vec<f64> = (0..l.n3)
                .map(|i| {
                    if free(i) {
                        -4.0 / (dt * dt * dt) * (u1[i] - u0[i] - dt * v0[i]) - 2.0 / (dt * dt) * v0[i]
                    } else {
                        -(2.0 * g_rate[i] - v0[i]) / (dt * dt)
                    }
                })
                .collect();
            let zc: Vec<f64> = (0..l.n3).map(|i| -(u1[i] - u0[i]) / (dt * dt)).collect();
            let (mz, cz) = (
                self.ctx.m.matvec(&zm).map_err(|e| CaeError::contract(e.to_string()))?,
                self.damping_matvec(&zc)?,
            );
            for i in (0..l.n3).filter(|i| free(*i)) {
                dtv[i] += mz[i] + cz[i];
            }
            acc_term = ak.clone();
            vel_term = (0..l.n3).map(|i| (u1[i] - u0[i]) / dt).collect();
        } else if c.inertia {
            let c1 = 1.0 / (c.beta * dt * dt);
            let c2 = 1.0 / (2.0 * c.beta) - 1.0;
            let g = c.gamma;
            let (cm_u1, cc_u1) = ((1.0 - c.am) * c1, (1.0 - c.af) * g * dt * c1);
            let dv = [g * dt * c1, -g * dt * c1, 1.0 - g * c1 * dt * dt, dt * ((1.0 - g) - g * c2)];
            let da = [c1, -c1, -c1 * dt, -c2];
            for i in (0..l.n3).filter(|i| free(*i)) {
                push_mc(&mut b.current, i, i, l.u(), (cm_u1, cc_u1), (0.0, 0.0));

                push_mc(
                    &mut b.previous,
                    i,
                    i,
                    l.u(),
                    ((1.0 - c.am) * da[1], (1.0 - c.af) * dv[1]),
                    (0.0, 0.0),
                );
                push_mc(
                    &mut b.previous,
                    i,
                    i,
                    l.v(),
                    ((1.0 - c.am) * da[2], (1.0 - c.af) * dv[2] + c.af),
                    (0.0, c.af),
                );
                push_mc(
                    &mut b.previous,
                    i,
                    i,
                    l.a(),
                    ((1.0 - c.am) * da[3] + c.am, (1.0 - c.af) * dv[3]),
                    (c.am, 0.0),
                );
                if c.af != 0.0 {
                    b.previous.push((i, l.r() + i, c.af));
                }
            }
            let mut dadt: Vec<f64> =
                (0..l.n3).map(|i| -(2.0 / dt) * (ak[i] + c2 * a0[i]) - c1 * v0[i]).collect();
            let mut dvdt: Vec<f64> =
                (0..l.n3).map(|i| (1.0 - g) * a0[i] + g * ak[i] + dt * g * dadt[i]).collect();
            for i in 0..l.n3 {
                b.current.push((l.v() + i, l.v() + i, 1.0));
                b.current.push((l.a() + i, l.a() + i, 1.0));
                if data_col(i) {
                    dvdt[i] = -vk[i] / dt;
                    dadt[i] = -2.0 * ak[i] / dt;
                } else {
                    b.current.push((l.v() + i, l.u() + i, -dv[0]));
                    b.previous.push((l.v() + i, l.u() + i, -dv[1]));
                    b.previous.push((l.v() + i, l.v() + i, -dv[2]));
                    b.previous.push((l.v() + i, l.a() + i, -dv[3]));
                    b.current.push((l.a() + i, l.u() + i, -da[0]));
                    b.previous.push((l.a() + i, l.u() + i, -da[1]));
                    b.previous.push((l.a() + i, l.v() + i, -da[2]));
                    b.previous.push((l.a() + i, l.a() + i, -da[3]));
                }
                dtv[l.v() + i] -= dvdt[i];
                dtv[l.a() + i] -= dadt[i];
            }
            let zm: Vec<f64> = dadt.iter().map(|v| (1.0 - c.am) * v).collect();
            let zc: Vec<f64> = dvdt.iter().map(|v| (1.0 - c.af) * v).collect();
            let (mz, cz) = (
                self.ctx.m.matvec(&zm).map_err(|e| CaeError::contract(e.to_string()))?,
                self.damping_matvec(&zc)?,
            );
            for i in (0..l.n3).filter(|i| free(*i)) {
                dtv[i] += mz[i] + cz[i];
            }
            acc_term = (0..l.n3).map(|i| (1.0 - c.am) * ak[i] + c.am * a0[i]).collect();
            vel_term = (0..l.n3).map(|i| (1.0 - c.af) * vk[i] + c.af * v0[i]).collect();
        } else {
            for i in 0..l.n3 {
                b.current.push((l.v() + i, l.v() + i, 1.0));
                b.current.push((l.a() + i, l.a() + i, 1.0));
                if c.af != 0.0 && free(i) {
                    b.previous.push((i, l.r() + i, c.af));
                }
            }
            acc_term = Vec::new();
            vel_term = Vec::new();
        }
        if c.inertia {
            for (e, t_) in m.mesh.elements.iter().enumerate() {
                let me0 = m.element_mass(e, 1.0);
                let ke = m.element_reference_stiffness(e);
                let dm = HyperDual::new(self.ctx.rho[e], 1.0, 0.0, 0.0);
                let dmass = m.interpolation.mass(dm).e1;
                let dstiff = m.interpolation.stiffness(dm).e1;
                for a in 0..4 {
                    for i in 0..3 {
                        let row = 3 * t_[a] + i;
                        if !free(row) {
                            continue;
                        }
                        let mut v = 0.0;
                        for bb in 0..4 {
                            let col = 3 * t_[bb] + i;
                            v += dmass * me0[a][bb] * (acc_term[col] + am * vel_term[col]);
                            for j in 0..3 {
                                v += dstiff
                                    * bk
                                    * ke[(3 * a + i) * 12 + 3 * bb + j]
                                    * vel_term[3 * t_[bb] + j];
                            }
                        }
                        b.params.push((row, e, v));
                    }
                }
            }
        }

        for i in 0..l.n3 {
            if !free(i) {
                b.current.push((i, i, 1.0));
            }
            b.current.push((l.r() + i, l.r() + i, 1.0));
        }

        if l.ne_visc > 0 {
            let siso: std::collections::HashMap<usize, [[f64; 14]; 6]> =
                elements.iter().filter_map(|(e, blk)| blk.siso.map(|s| (*e, s))).collect();
            let taus = |e: usize| {
                m.materials[m.element_material[e]].prony.as_ref().map(|p| p.tau.clone()).unwrap_or_default()
            };
            for e in 0..ne {
                let (factors, _) = self.branch_factors(e, dt);
                let dofs = m.element_dofs(e);
                for k in 0..6 {
                    let row = l.s() + 6 * e + k;
                    b.current.push((row, row, 1.0));
                    if let Some(sj) = siso.get(&e) {
                        for j in 0..12 {
                            b.current.push((row, l.u() + dofs[j], -sj[k][j]));
                        }
                        b.params.push((row, e, -sj[k][12]));
                        b.params.push((row, ne + e, -sj[k][13]));
                    }
                }
                let tau = taus(e);
                for i in 0..l.nb {
                    let off = l.q() + 6 * (e * l.nb + i);
                    for k in 0..6 {
                        b.current.push((off + k, off + k, 1.0));
                        if let Some((ei, bi)) = factors.get(i) {
                            b.current.push((off + k, l.s() + 6 * e + k, -bi));
                            b.previous.push((off + k, off + k, -ei));
                            b.previous.push((off + k, l.s() + 6 * e + k, *bi));
                            let (dei, dbi) = (-ei / tau[i], -0.5 * bi / tau[i]);
                            dtv[off + k] -= dei * prev[off + k]
                                + dbi * (cur[l.s() + 6 * e + k] - prev[l.s() + 6 * e + k]);
                        }
                    }
                }
            }
        }
        Ok(StateJacobians {
            residual,
            current: csr(n, n, &b.current)?,
            previous: csr(n, n, if current_only { &[] } else { &b.previous })?,
            params: csr(n, 2 * ne, if current_only { &[] } else { &b.params })?,
            extra: csr(n, l.n3, if current_only { &[] } else { &b.extra })?,
            dt: dtv,
        })
    }
}
