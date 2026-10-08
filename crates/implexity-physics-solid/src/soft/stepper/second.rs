// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::{Dual, HyperDual, Jet3, Scalar};
use implexity_core::CaeError;

use super::state::Rule;
use super::{Scheme, SoftHistory};
use crate::util::contract;

#[derive(Debug, Clone)]
pub struct StateAdjointTangent {
    pub current: Vec<f64>,
    pub previous: Vec<f64>,
    pub params: Vec<f64>,
    pub extra: Vec<f64>,
    pub dt: f64,
}

struct KinCoeffs<S> {
    x: [S; 4],
    vm: [S; 4],
    vk: [S; 4],
    ak: [S; 4],
}

fn kin_coeffs<S: Scalar>(scheme: Scheme, dt: S) -> KinCoeffs<S> {
    let z = S::zero();
    let one = S::one();
    match scheme {
        Scheme::Quasistatic => KinCoeffs { x: [z; 4], vm: [z; 4], vk: [z; 4], ak: [z; 4] },
        Scheme::AvfMidpoint { .. } => {
            let r = one / dt;
            let r2 = r * r;
            KinCoeffs {
                x: [r2 * 2.0, r2 * (-2.0), r * (-2.0), z],
                vm: [r, -r, z, z],
                vk: [r * 2.0, r * (-2.0), -one, z],
                ak: [r2 * 2.0, r2 * (-2.0), r * (-2.0), z],
            }
        }
        Scheme::Newmark { .. } | Scheme::GeneralizedAlpha { .. } => {
            let c = scheme.coeffs();
            let c1 = one / (dt * dt * c.beta);
            let c2 = 1.0 / (2.0 * c.beta) - 1.0;
            let g = c.gamma;
            let ak = [c1, -c1, -(c1 * dt), S::from_f64(-c2)];
            let vk = [dt * c1 * g, -(dt * c1 * g), one - c1 * dt * dt * g, dt * ((1.0 - g) - g * c2)];
            let x = [
                ak[0] * (1.0 - c.am),
                ak[1] * (1.0 - c.am),
                ak[2] * (1.0 - c.am),
                ak[3] * (1.0 - c.am) + c.am,
            ];
            let vm = [
                vk[0] * (1.0 - c.af),
                vk[1] * (1.0 - c.af),
                vk[2] * (1.0 - c.af) + c.af,
                vk[3] * (1.0 - c.af),
            ];
            KinCoeffs { x, vm, vk, ak }
        }
    }
}

fn kin_coeffs_prescribed<S: Scalar>(scheme: Scheme, dt: S) -> KinCoeffs<S> {
    let z = S::zero();
    match scheme {
        Scheme::Quasistatic => KinCoeffs { x: [z; 4], vm: [z; 4], vk: [z; 4], ak: [z; 4] },
        Scheme::AvfMidpoint { .. } => {
            let r = S::one() / dt;
            KinCoeffs { x: [z, z, -r, z], vm: [r, -r, z, z], vk: [z; 4], ak: [z, z, -r, z] }
        }
        Scheme::Newmark { .. } | Scheme::GeneralizedAlpha { .. } => {
            let c = scheme.coeffs();
            KinCoeffs {
                x: [z, z, z, S::from_f64(c.am)],
                vm: [z, z, S::from_f64(c.af), z],
                vk: [z; 4],
                ak: [z; 4],
            }
        }
    }
}

struct ElementSecond {
    cur: [f64; 16],
    prev: [f64; 16],
    params: [f64; 2],
    h: [f64; 6],
    dt: f64,
    siso: Option<[f64; 14]>,
}

struct Pass<'a> {
    rule: &'a Rule,
    prev: &'a [f64],
    u0: &'a [f64],
    p0: &'a [f64],
    u1: &'a [f64],
    p1: &'a [f64],
    du0: &'a [f64],
    dp0: &'a [f64],
    du1: &'a [f64],
    dp1: &'a [f64],
    d_prev: &'a [f64],
    dparams: &'a [f64],
    omega_u: &'a [f64],
    omega_p: &'a [f64],
    w: &'a [f64],
    dt: f64,
}

fn third_unavailable(law: &str) -> CaeError {
    CaeError::contract(format!(
        "the {law} law has no third derivatives (a spectral trace lifted with second derivatives only): \
         second-order adjoints of the step (growth-rate gradients) are unavailable for this model"
    ))
}

impl SoftHistory<'_> {
    fn history_h_dt(&self, x: &[f64], e: usize, dt: f64) -> [f64; 6] {
        let l = self.layout;
        let mut out = [0.0; 6];
        if l.ne_visc == 0 {
            return out;
        }
        let m = self.model;
        let (factors, _) = self.branch_factors(e, dt);
        let Some(prony) = m.materials[m.element_material[e]].prony.as_ref() else {
            return out;
        };
        let s = &x[l.s() + 6 * e..l.s() + 6 * e + 6];
        for (i, ((ei, bi), tau)) in factors.iter().zip(&prony.tau).enumerate() {
            let (dei, dbi) = (-ei / tau, -0.5 * bi / tau);
            let q = &x[l.q() + 6 * (e * l.nb + i)..l.q() + 6 * (e * l.nb + i) + 6];
            for k in 0..6 {
                out[k] += dei * q[k] - dbi * s[k];
            }
        }
        out
    }

    #[allow(clippy::too_many_lines)]
    fn second_element(&self, e: usize, p: &Pass<'_>) -> Result<ElementSecond, CaeError> {
        let m = self.model;
        let ne = m.ne();
        let d0 = m.gather(e, p.u0, p.p0);
        let d1 = m.gather(e, p.u1, p.p1);
        let dd0 = m.gather(e, p.du0, p.dp0);
        let dd1 = m.gather(e, p.du1, p.dp1);
        let om = m.gather(e, p.omega_u, p.omega_p);
        let h = self.history_h(p.prev, e, p.dt);
        let dh = self.history_h(p.d_prev, e, p.dt);
        let (factors, gf) = self.branch_factors(e, p.dt);
        let visc = self.layout.ne_visc > 0 && !factors.is_empty();
        let (h_dt, dh_dt, dg) = if visc {
            let tau = m.materials[m.element_material[e]]
                .prony
                .as_ref()
                .map(|pr| pr.tau.clone())
                .unwrap_or_default();
            let dg: f64 = factors.iter().zip(&tau).map(|((_, bi), t)| -0.5 * bi / t).sum();
            (self.history_h_dt(p.prev, e, p.dt), self.history_h_dt(p.d_prev, e, p.dt), dg)
        } else {
            ([0.0; 6], [0.0; 6], 0.0)
        };
        let (rho, theta) = (self.ctx.rho[e], self.ctx.theta[e]);
        let (drho, dtheta) = (p.dparams[e], p.dparams[ne + e]);
        let law = || m.materials[m.element_material[e]].law.name().to_string();
        let mut out = ElementSecond {
            cur: [0.0; 16],
            prev: [0.0; 16],
            params: [0.0; 2],
            h: [0.0; 6],
            dt: 0.0,
            siso: None,
        };
        for (sg, wg) in p.rule.s.iter().zip(&p.rule.w) {
            let (d, dd): ([f64; 16], [f64; 16]) = if p.rule.avf {
                (
                    core::array::from_fn(|k| d0[k] + sg * (d1[k] - d0[k])),
                    core::array::from_fn(|k| dd0[k] + sg * (dd1[k] - dd0[k])),
                )
            } else {
                (d1, dd1)
            };
            let mut z = [0.0; 24];
            let mut a = [0.0; 24];
            let mut b = [0.0; 24];
            z[..16].copy_from_slice(&d);
            a[..16].copy_from_slice(&om);
            b[..16].copy_from_slice(&dd);
            (z[16], z[17], b[16], b[17]) = (rho, theta, drho, dtheta);
            z[18..].copy_from_slice(&h);
            b[18..].copy_from_slice(&dh);
            let jet = Jet3::<24>::seed(&z, &a, &b);
            let dl: [Jet3<24>; 16] = core::array::from_fn(|k| jet[k]);
            let hj: [Jet3<24>; 6] = core::array::from_fn(|k| jet[18 + k]);
            let pi = m.element_energy(e, &dl, jet[16], jet[17], &hj, gf)?;
            if !pi.third_is_finite() {
                return Err(third_unavailable(&law()));
            }
            let t3 = pi.third();
            for k in 0..16 {
                out.cur[k] += wg * sg * t3[k];
                if p.rule.avf {
                    out.prev[k] += wg * (1.0 - sg) * t3[k];
                }
            }
            out.params[0] += wg * t3[16];
            out.params[1] += wg * t3[17];
            for k in 0..6 {
                out.h[k] += wg * t3[18 + k];
            }
            if visc {

                let ga = pi.gradient_d1();
                let mut v = 0.0;
                for k in 0..6 {
                    v += t3[18 + k] * h_dt[k] + ga[18 + k] * dh_dt[k];
                }
                let mixed = |g: f64| -> Result<f64, CaeError> {
                    let hd: [HyperDual; 24] = core::array::from_fn(|k| HyperDual::new(z[k], a[k], b[k], 0.0));
                    let dl: [HyperDual; 16] = core::array::from_fn(|k| hd[k]);
                    let hh: [HyperDual; 6] = core::array::from_fn(|k| hd[18 + k]);
                    Ok(m.element_energy(e, &dl, hd[16], hd[17], &hh, g)?.e12)
                };
                v += dg * (mixed(1.0)? - mixed(0.0)?);
                out.dt += wg * v;
            }
        }
        if visc {

            let l = self.layout;
            let ws = &p.w[l.s() + 6 * e..l.s() + 6 * e + 6];
            let wt: [f64; 6] = core::array::from_fn(|k| if k < 3 { ws[k] } else { 0.5 * ws[k] });
            let mut z = [0.0; 15];
            let mut a = [0.0; 15];
            let mut b = [0.0; 15];
            z[..12].copy_from_slice(&d1[..12]);
            b[..12].copy_from_slice(&dd1[..12]);
            (z[12], z[13], b[12], b[13]) = (rho, theta, drho, dtheta);
            a[14] = 1.0;
            let jet = Jet3::<15>::seed(&z, &a, &b);
            let ul: [[Jet3<15>; 3]; 4] = core::array::from_fn(|n| core::array::from_fn(|i| jet[3 * n + i]));
            let f = m.isochoric_stress_dot(e, &ul, jet[12], jet[13], &wt, jet[14]);
            if !f.third_is_finite() {
                return Err(third_unavailable(&law()));
            }
            out.siso = Some(core::array::from_fn(|k| -f.third()[k]));
        }
        Ok(out)
    }


    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn state_adjoint_tangent(
        &self,
        t: usize,
        prev: &[f64],
        cur: &[f64],
        extra: Option<&[f64]>,
        w: &[f64],
        d_cur: &[f64],
        d_prev: &[f64],
        d_params: Option<&[f64]>,
    ) -> Result<StateAdjointTangent, CaeError> {
        self.check_states(t, prev, cur, extra)?;
        let m = self.model;
        let l = self.layout;
        let (n, ne, n3, np) = (l.len(), m.ne(), l.n3, l.np);
        if w.len() != n || d_cur.len() != n || d_prev.len() != n {
            return contract("second-order step residual: cotangent or direction length mismatch");
        }
        let zeros = vec![0.0; 2 * ne];
        let dparams = d_params.unwrap_or(&zeros);
        if dparams.len() != 2 * ne {
            return contract("second-order step residual: design direction length mismatch");
        }
        let dt = self.dt(t);
        let rule = self.state_rule()?;
        let free = |i: usize| !m.fixed[i];
        let omega_u: Vec<f64> =
            (0..n3).map(|i| if free(i) { rule.wu * w[l.u() + i] } else { 0.0 } - w[l.r() + i]).collect();
        let omega_p: Vec<f64> = (0..np).map(|a| rule.wu * w[l.p() + a]).collect();
        let slice = |x: &'_ [f64], off: usize, len: usize| -> Vec<f64> { x[off..off + len].to_vec() };
        let (u0, p0, u1, p1) =
            (slice(prev, l.u(), n3), slice(prev, l.p(), np), slice(cur, l.u(), n3), slice(cur, l.p(), np));
        let (du0, dp0, du1, dp1) = (
            slice(d_prev, l.u(), n3),
            slice(d_prev, l.p(), np),
            slice(d_cur, l.u(), n3),
            slice(d_cur, l.p(), np),
        );
        let mut out = StateAdjointTangent {
            current: vec![0.0; n],
            previous: vec![0.0; n],
            params: vec![0.0; 2 * ne],
            extra: vec![0.0; n3],
            dt: 0.0,
        };

        let pass = Pass {
            rule: &rule,
            prev,
            u0: &u0,
            p0: &p0,
            u1: &u1,
            p1: &p1,
            du0: &du0,
            dp0: &dp0,
            du1: &du1,
            dp1: &dp1,
            d_prev,
            dparams,
            omega_u: &omega_u,
            omega_p: &omega_p,
            w,
            dt,
        };
        let state_of = |dofs: &[usize; 16], k: usize| -> usize {
            if k < 12 { l.u() + dofs[k] } else { l.p() + dofs[k] - n3 }
        };
        let nl = m.nl();
        m.for_elements(
            |e| self.second_element(e, &pass),
            |e, blk| {
                let dofs = m.element_dofs(e);
                for k in 0..nl {
                    let col = state_of(&dofs, k);
                    out.current[col] += blk.cur[k];
                    out.previous[col] += blk.prev[k];
                }
                out.params[e] += blk.params[0];
                out.params[ne + e] += blk.params[1];
                if l.ne_visc > 0 {
                    let (factors, _) = self.branch_factors(e, dt);
                    let sum_b: f64 = factors.iter().map(|(_, bi)| bi).sum();
                    for k in 0..6 {
                        for (i, (ei, _)) in factors.iter().enumerate() {
                            out.previous[l.q() + 6 * (e * l.nb + i) + k] += blk.h[k] * ei;
                        }
                        out.previous[l.s() + 6 * e + k] -= blk.h[k] * sum_b;
                    }
                }
                out.dt += blk.dt;
                if let Some(s) = blk.siso {
                    for k in 0..12 {
                        out.current[l.u() + dofs[k]] += s[k];
                    }
                    out.params[e] += s[12];
                    out.params[ne + e] += s[13];
                }
                Ok(())
            },
        )?;

        let path = |a: &[f64], b: &[f64], sg: f64| -> Vec<f64> {
            if rule.avf { a.iter().zip(b).map(|(x, y)| x + sg * (y - x)).collect() } else { b.to_vec() }
        };
        for pot in &m.potentials {
            for (sg, wg) in rule.s.iter().zip(&rule.w) {
                let (gu, gp) = pot.force_second_directional(
                    &path(&u0, &u1, *sg),
                    &self.params,
                    &omega_u,
                    &path(&du0, &du1, *sg),
                    dparams,
                )?;
                if gu.len() != n3 || gp.len() != 2 * ne {
                    return contract("nodal potential second-order pullback has the wrong length");
                }
                for i in 0..n3 {
                    out.current[l.u() + i] += wg * sg * gu[i];
                    if rule.avf {
                        out.previous[l.u() + i] += wg * (1.0 - sg) * gu[i];
                    }
                }
                for (o, g) in out.params.iter_mut().zip(&gp) {
                    *o += wg * g;
                }
            }
        }
        let pressure = if rule.avf { self.avf_pressure(t) } else { self.pressure(t) };
        if pressure != 0.0 {
            for (f, face) in m.faces.iter().enumerate() {
                let dofs: [usize; 9] = core::array::from_fn(|k| 3 * face[k / 3] + k % 3);
                for (sg, wg) in rule.s.iter().zip(&rule.w) {
                    let ug = path(&u0, &u1, *sg);
                    let dug = path(&du0, &du1, *sg);
                    let x = m.face_positions(f, &ug);
                    let xv: [f64; 9] = core::array::from_fn(|k| x[k / 3][k % 3]);
                    let a: [f64; 9] = core::array::from_fn(|k| dug[dofs[k]]);
                    let jet = Jet3::<9>::seed(&xv, &a, &[0.0; 9]);
                    let xj: [[Jet3<9>; 3]; 3] =
                        core::array::from_fn(|k| core::array::from_fn(|i| jet[3 * k + i]));
                    let r = crate::soft::model::SoftModel::face_residual(&xj, pressure);
                    let mut psi = Jet3::<9>::constant(0.0);
                    for node in face {
                        for (i, ri) in r.iter().enumerate() {
                            psi += *ri * omega_u[3 * node + i];
                        }
                    }
                    let g1 = psi.gradient_d1();
                    for k in 0..9 {
                        out.current[l.u() + dofs[k]] += wg * sg * g1[k];
                        if rule.avf {
                            out.previous[l.u() + dofs[k]] += wg * (1.0 - sg) * g1[k];
                        }
                    }
                }
            }
        }

        let dual_dt = Dual::<1>::variable(dt, 0);
        let (kin, kin_fixed) =
            (kin_coeffs(self.scheme, dual_dt), kin_coeffs_prescribed(self.scheme, dual_dt));
        let val = |c: &[Dual<1>; 4]| -> [f64; 4] { c.map(|v| v.re) };
        let der = |c: &[Dual<1>; 4]| -> [f64; 4] { c.map(|v| v.eps[0]) };
        let data_col = |i: usize| self.prescribes_support_rates() && m.fixed[i];

        let (cx, cvm) = ((val(&kin.x), val(&kin_fixed.x)), (val(&kin.vm), val(&kin_fixed.vm)));
        let (cvk, cak) = ((der(&kin.vk), der(&kin_fixed.vk)), (der(&kin.ak), der(&kin_fixed.ak)));
        let (cx_dt, cvm_dt) = ((der(&kin.x), der(&kin_fixed.x)), (der(&kin.vm), der(&kin_fixed.vm)));

        let (mut x_data, mut vm_data) = (vec![(0.0, 0.0); n3], vec![(0.0, 0.0); n3]);
        if self.prescribes_support_rates() {
            let (g, h) = self.prescribed_rates(t);
            let c = self.scheme.coeffs();
            for i in (0..n3).filter(|i| m.fixed[*i]) {
                let scale = Dual::<1>::constant(dt) / dual_dt;
                let (gd, hd) = (scale * g[i], scale * scale * h[i]);
                let (xd, vd) = if rule.avf {
                    (gd / dual_dt, Dual::constant(0.0))
                } else {
                    (hd * (1.0 - c.am), gd * (1.0 - c.af))
                };
                x_data[i] = (xd.re, xd.eps[0]);
                vm_data[i] = (vd.re, vd.eps[0]);
            }
        }
        let states = [&u1[..], &u0[..], &prev[l.v()..l.v() + n3], &prev[l.a()..l.a() + n3]];
        let dstates = [&du1[..], &du0[..], &d_prev[l.v()..l.v() + n3], &d_prev[l.a()..l.a() + n3]];
        let pick = |c: &([f64; 4], [f64; 4]), i: usize| -> [f64; 4] { if data_col(i) { c.1 } else { c.0 } };
        let combo = |c: &([f64; 4], [f64; 4]), s: &[&[f64]; 4]| -> Vec<f64> {
            (0..n3)
                .map(|i| {
                    let k = pick(c, i);
                    k[0] * s[0][i] + k[1] * s[1][i] + k[2] * s[2][i] + k[3] * s[3][i]
                })
                .collect()
        };
        let with_data = |mut v: Vec<f64>, data: &[(f64, f64)], derivative: bool| -> Vec<f64> {
            for (a, d) in v.iter_mut().zip(data) {
                *a += if derivative { d.1 } else { d.0 };
            }
            v
        };
        let dot = |a: &[f64], b: &[f64]| -> f64 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        let inertia = rule.avf || self.scheme.coeffs().inertia;
        if inertia {
            let (am, bk) = self.rayleigh;
            let wf: Vec<f64> = (0..n3).map(|i| if free(i) { w[l.u() + i] } else { 0.0 }).collect();
            let (xs, vs) = (
                with_data(combo(&cx, &states), &x_data, false),
                with_data(combo(&cvm, &states), &vm_data, false),
            );
            let (dxs, dvs) = (combo(&cx, &dstates), combo(&cvm, &dstates));
            let mut gm = vec![0.0; n3];
            let mut gk = vec![0.0; n3];
            for (e, tet) in m.mesh.elements.iter().enumerate() {
                let me0 = m.element_mass(e, 1.0);
                let ke = m.element_reference_stiffness(e);
                let hd = HyperDual::new(self.ctx.rho[e], 1.0, 1.0, 0.0);
                let (mass, stiff) = (m.interpolation.mass(hd), m.interpolation.stiffness(hd));
                let drho = dparams[e];

                let mprod = |y: &[f64]| -> f64 {
                    let mut s = 0.0;
                    for a in 0..4 {
                        for b in 0..4 {
                            for i in 0..3 {
                                s += wf[3 * tet[a] + i] * me0[a][b] * y[3 * tet[b] + i];
                            }
                        }
                    }
                    s
                };
                let kprod = |y: &[f64]| -> f64 {
                    let mut s = 0.0;
                    for a in 0..4 {
                        for i in 0..3 {
                            for b in 0..4 {
                                for j in 0..3 {
                                    s += wf[3 * tet[a] + i]
                                        * ke[(3 * a + i) * 12 + 3 * b + j]
                                        * y[3 * tet[b] + j];
                                }
                            }
                        }
                    }
                    s
                };
                let xv: Vec<f64> = xs.iter().zip(&vs).map(|(x, v)| x + am * v).collect();
                let dxv: Vec<f64> = dxs.iter().zip(&dvs).map(|(x, v)| x + am * v).collect();
                out.params[e] += mass.e12 * drho * mprod(&xv)
                    + mass.e1 * mprod(&dxv)
                    + bk * (stiff.e12 * drho * kprod(&vs) + stiff.e1 * kprod(&dvs));
                if drho != 0.0 {
                    for a in 0..4 {
                        for i in 0..3 {
                            let row = 3 * tet[a] + i;
                            for b in 0..4 {
                                gm[3 * tet[b] + i] += mass.e1 * drho * me0[a][b] * wf[row];
                                for j in 0..3 {
                                    gk[3 * tet[b] + j] +=
                                        stiff.e1 * drho * ke[(3 * a + i) * 12 + 3 * b + j] * wf[row];
                                }
                            }
                        }
                    }
                }
            }
            let qv: Vec<f64> = gm.iter().zip(&gk).map(|(a, b)| am * a + bk * b).collect();
            let targets = [(l.u(), true), (l.u(), false), (l.v(), false), (l.a(), false)];
            for (k, (off, current)) in targets.iter().enumerate() {
                let dst = if *current { &mut out.current } else { &mut out.previous };
                for i in 0..n3 {
                    dst[off + i] += pick(&cx, i)[k] * gm[i] + pick(&cvm, i)[k] * qv[i];
                }
            }

            let (x_dt, v_dt) = (
                with_data(combo(&cx_dt, &states), &x_data, true),
                with_data(combo(&cvm_dt, &states), &vm_data, true),
            );
            let (dx_dt, dv_dt) = (combo(&cx_dt, &dstates), combo(&cvm_dt, &dstates));
            let mv = |v: &[f64]| self.ctx.m.matvec(v).map_err(|e| CaeError::contract(e.to_string()));
            let kv = |v: &[f64]| self.ctx.kref.matvec(v).map_err(|e| CaeError::contract(e.to_string()));
            let dxv_dt: Vec<f64> = dx_dt.iter().zip(&dv_dt).map(|(x, v)| x + am * v).collect();
            let xv_dt: Vec<f64> = x_dt.iter().zip(&v_dt).map(|(x, v)| x + am * v).collect();
            out.dt += dot(&wf, &mv(&dxv_dt)?)
                + bk * dot(&wf, &kv(&dv_dt)?)
                + dot(&gm, &xv_dt)
                + bk * dot(&gk, &v_dt);

            let (vk_dt, ak_dt) = (combo(&cvk, &dstates), combo(&cak, &dstates));
            out.dt -= dot(&w[l.v()..l.v() + n3], &vk_dt) + dot(&w[l.a()..l.a() + n3], &ak_dt);
        }

        if l.ne_visc > 0 {
            for e in 0..ne {
                let (factors, _) = self.branch_factors(e, dt);
                let Some(prony) = m.materials[m.element_material[e]].prony.as_ref() else { continue };
                for (i, ((ei, bi), tau)) in factors.iter().zip(&prony.tau).enumerate() {
                    let (dei, dbi) = (-ei / tau, -0.5 * bi / tau);
                    let off = l.q() + 6 * (e * l.nb + i);
                    for k in 0..6 {
                        let ds = d_cur[l.s() + 6 * e + k] - d_prev[l.s() + 6 * e + k];
                        out.dt -= w[off + k] * (dei * d_prev[off + k] + dbi * ds);
                    }
                }
            }
        }
        Ok(out)
    }
}
