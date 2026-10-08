// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use std::sync::Arc;

use implexity_ad::AdError;

use super::ad::{A, Graph, Shape};
use super::darcy::{Coolant, FLOW_AXIS_DEFAULT, Lam, PressurePorts, TINY, faces, port_face_flux};
use super::grid::flat;
use super::ops::{concat_axis, embed, hi_part, lo_part, plane, slice_axis};
use super::stencil::CellPattern;

pub const K_AXIAL_FLOOR_REL: f64 = 1e-9;

#[must_use]
pub fn face_fluxes<'g>(
    lam: &Lam<'g>,
    h: f64,
    p: A<'g>,
    dp: A<'g>,
    flow_axis: usize,
    ports: Option<&PressurePorts>,
) -> [A<'g>; 3] {
    let g = p.graph();
    let s = p.shape().as3();
    let t = faces(lam, h);
    let interior: [A<'g>; 3] = std::array::from_fn(|a| t[a] * (lo_part(p, a) - hi_part(p, a)));
    let port = ports.map(|pp| port_face_flux(lam, h, p, dp, pp));
    std::array::from_fn(|a| {
        let mut sh = s;
        sh[a] = 1;
        let zero = || g.full(0.0, Shape::d3(sh));
        let (f_lo, f_hi) = match &port {
            Some((pa, side, f)) if *pa == a => {
                if *side == 0 {
                    (*f, zero())
                } else {
                    (zero(), *f)
                }
            }
            #[allow(clippy::match_same_arms)]
            Some(_) => (zero(), zero()),
            None if a == flow_axis => {
                let lo = plane(lam.axes[a], a, false) * (dp - plane(p, a, false)) * (2.0 * h);
                let hi = plane(lam.axes[a], a, true) * plane(p, a, true) * (2.0 * h);
                (lo, hi)
            }
            None => (zero(), zero()),
        };
        concat_axis(&[f_lo, interior[a], f_hi], a)
    })
}

#[must_use]
pub fn axial_conductivity<'g>(eps: A<'g>, coolant: &Coolant) -> A<'g> {
    let kf = coolant.k_f;
    eps.map(move |e| (e + K_AXIAL_FLOOR_REL) * kf)
}

#[derive(Clone, Copy)]
pub struct FluidParams<'g> {
    pub u_face: [A<'g>; 3],
    pub g_vol: A<'g>,
    pub k_axial: Option<A<'g>>,
}

#[derive(Clone, Debug)]
pub struct FluidSetup {
    pub c: f64,
    pub t_in: f64,
    pub k_f: f64,
    pub h: f64,
    pub flow_axis: usize,
    pub ports: Option<PressurePorts>,
}

fn kd_field<'g>(p: &FluidParams<'g>, setup: &FluidSetup, s: [usize; 3]) -> A<'g> {
    match p.k_axial {
        Some(k) => k,
        None => p.g_vol.graph().full(setup.k_f, Shape::d3(s)),
    }
}

type Terms<'g> = Vec<(usize, bool, A<'g>)>;

fn boundary_terms<'g>(p: &FluidParams<'g>, setup: &FluidSetup) -> (Terms<'g>, Terms<'g>) {
    let c = setup.c;
    let t_in = setup.t_in;
    let mut bnd = Vec::new();
    let mut loads = Vec::new();
    match &setup.ports {
        None => {
            let fa = setup.flow_axis;
            let f_in = plane(p.u_face[fa], fa, false);
            let f_in_p = f_in.max_c(0.0);
            let f_in_m = f_in.min_c(0.0);
            for a in 0..3 {
                let lo = if a == fa { f_in_m } else { plane(p.u_face[a], a, false) };
                bnd.push((a, false, lo * (-c)));
                bnd.push((a, true, plane(p.u_face[a], a, true) * c));
            }
            loads.push((fa, false, f_in_p * (c * t_in)));
        }
        Some(_) => {
            for a in 0..3 {
                let flo = plane(p.u_face[a], a, false);
                let fhi = plane(p.u_face[a], a, true);
                let (lo_in, lo_out) = (flo.max_c(0.0), flo.min_c(0.0));
                let (hi_in, hi_out) = (fhi.min_c(0.0), fhi.max_c(0.0));
                bnd.push((a, false, lo_out * (-c)));
                bnd.push((a, true, hi_out * c));
                loads.push((a, false, lo_in * (c * t_in)));
                loads.push((a, true, hi_in * (-c * t_in)));
            }
        }
    }
    (bnd, loads)
}

fn plane_lo(s: [usize; 3], a: usize, last: bool) -> [usize; 3] {
    let mut lo = [0; 3];
    if last {
        lo[a] = s[a] - 1;
    }
    lo
}

#[must_use]
pub fn apply<'g>(p: &FluidParams<'g>, setup: &FluidSetup, t: A<'g>) -> A<'g> {
    let s = p.g_vol.shape().as3();
    let c = setup.c;
    let kd = kd_field(p, setup, s);
    let d = faces(&Lam::iso(kd), setup.h);
    let mut out = p.g_vol * t;
    for a in 0..3 {
        let n = p.u_face[a].shape().as3()[a];
        let fi = slice_axis(p.u_face[a], a, 1, n - 1);
        let fp = fi.max_c(0.0);
        let fm = fi.min_c(0.0);
        let tl = lo_part(t, a);
        let tr = hi_part(t, a);
        let flux = (fp * tl + fm * tr) * c + d[a] * (tl - tr);
        let mut hi_lo = [0; 3];
        hi_lo[a] = 1;
        out = out + embed(flux, [0; 3], s) - embed(flux, hi_lo, s);
    }
    let (bnd, _) = boundary_terms(p, setup);
    for (a, last, coef) in bnd {
        let tp = plane(t, a, last);
        out = out + embed(coef * tp, plane_lo(s, a, last), s);
    }
    out
}

#[must_use]
pub fn load<'g>(p: &FluidParams<'g>, setup: &FluidSetup, t_solid: A<'g>) -> A<'g> {
    let s = p.g_vol.shape().as3();
    let mut b = p.g_vol * t_solid;
    let (_, loads) = boundary_terms(p, setup);
    for (a, last, v) in loads {
        b = b + embed(v, plane_lo(s, a, last), s);
    }
    b
}

#[must_use]
pub fn assemble(pat: &CellPattern, p: &FluidParams<'_>, setup: &FluidSetup) -> Vec<f64> {
    let s = pat.shape;
    let c = setup.c;
    let h = setup.h;
    let gv = p.g_vol.val();
    let kd = p.k_axial.map_or_else(|| vec![setup.k_f; pat.n], |k| k.val());
    let uf: [Vec<f64>; 3] = std::array::from_fn(|a| p.u_face[a].val());
    let mut data = pat.zeros();
    for cell in 0..pat.n {
        data[pat.diag[cell]] += gv[cell];
    }
    for a in 0..3 {
        let mut fs = s;
        fs[a] += 1;
        for i in 0..s[0] {
            for j in 0..s[1] {
                for k in 0..s[2] {
                    let pt = [i, j, k];
                    if pt[a] + 1 >= s[a] {
                        continue;
                    }
                    let mut q = pt;
                    q[a] += 1;
                    let (c0, c1) = (flat(s, i, j, k), flat(s, q[0], q[1], q[2]));
                    let fv = uf[a][flat(fs, q[0], q[1], q[2])];
                    let (fp, fm) = (fv.max(0.0), fv.min(0.0));
                    let (x, y) = (kd[c0], kd[c1]);
                    let dd = 2.0 * h * x * y / (x + y + TINY);
                    data[pat.diag[c0]] += c * fp + dd;
                    data[pat.slot(c0, c1)] += c * fm - dd;
                    data[pat.slot(c1, c0)] -= c * fp + dd;
                    data[pat.diag[c1]] += -c * fm + dd;
                }
            }
        }
    }
    let add_plane = |data: &mut Vec<f64>, a: usize, last: bool, coef: &dyn Fn(f64) -> f64| {
        let mut fs = s;
        fs[a] += 1;
        let idx = if last { s[a] - 1 } else { 0 };
        let fidx = if last { s[a] } else { 0 };
        for i in 0..s[0] {
            for j in 0..s[1] {
                for k in 0..s[2] {
                    let pt = [i, j, k];
                    if pt[a] != idx {
                        continue;
                    }
                    let mut fq = pt;
                    fq[a] = fidx;
                    let fv = uf[a][flat(fs, fq[0], fq[1], fq[2])];
                    data[pat.diag[flat(s, i, j, k)]] += coef(fv);
                }
            }
        }
    };
    match &setup.ports {
        None => {
            let fa = setup.flow_axis;
            for a in 0..3 {
                if a == fa {
                    add_plane(&mut data, a, false, &|f| -c * f.min(0.0));
                } else {
                    add_plane(&mut data, a, false, &|f| -c * f);
                }
                add_plane(&mut data, a, true, &|f| c * f);
            }
        }
        Some(_) => {
            for a in 0..3 {
                add_plane(&mut data, a, false, &|f| -c * f.min(0.0));
                add_plane(&mut data, a, true, &|f| c * f.max(0.0));
            }
        }
    }
    data
}

#[must_use]
pub fn solve_fluid_temperature<'g>(p: &FluidParams<'g>, setup: &FluidSetup, t_solid: A<'g>) -> A<'g> {
    let g = t_solid.graph();
    let s = p.g_vol.shape().as3();
    let b = load(p, setup, t_solid.reshape(Shape::d3(s)));
    let pat = CellPattern::of(s);
    let data = assemble(&pat, p, setup);
    let fact = match pat.lu_of_transpose(data.clone()) {
        Ok(f) => Arc::new(f),
        Err(e) => return g.failed(&format!("fluid solve: {e}"), Shape::d3(s)),
    };
    let bv = b.val();
    let x = match fact.solve_transpose(&bv) {
        Ok(x) => x,
        Err(e) => return g.failed(&format!("fluid solve: {e}"), Shape::d3(s)),
    };
    let ax = pat.matvec(&data, &x);
    let x = super::darcy::checked(x, &ax, &bv);
    let xs = Arc::new(x.clone());
    let uv: Arc<[Vec<f64>; 3]> = Arc::new(std::array::from_fn(|a| p.u_face[a].val()));
    let ushape: [Shape; 3] = std::array::from_fn(|a| p.u_face[a].shape());
    let gv = Arc::new(p.g_vol.val());
    let kv = p.k_axial.map(|k| Arc::new(k.val()));
    let setup2 = setup.clone();
    let mut inputs = vec![p.u_face[0], p.u_face[1], p.u_face[2], p.g_vol];
    if let Some(k) = p.k_axial {
        inputs.push(k);
    }
    inputs.push(b);
    g.custom(
        &inputs,
        x,
        Shape::d3(s),
        Box::new(move |gx: &[f64]| {
            let lam = fact.solve(gx).map_err(|e| AdError::Callback(e.to_string()))?;
            let sub = Graph::new();
            let uf: [A<'_>; 3] = std::array::from_fn(|a| sub.input(uv[a].clone(), ushape[a]));
            let gg = sub.input(gv.as_ref().clone(), Shape::d3(s));
            let kk = kv.as_ref().map(|k| sub.input(k.as_ref().clone(), Shape::d3(s)));
            let pp = FluidParams { u_face: uf, g_vol: gg, k_axial: kk };
            let tc = sub.constant(xs.as_ref().clone(), Shape::d3(s));
            let y = apply(&pp, &setup2, tc);
            let mut wrt = vec![uf[0], uf[1], uf[2], gg];
            if let Some(k) = kk {
                wrt.push(k);
            }
            let gr = sub.vjp(y, &lam, &wrt).map_err(AdError::Callback)?;
            let mut out: Vec<Vec<f64>> =
                gr.into_iter().map(|v| v.into_iter().map(|x| -x).collect()).collect();
            out.push(lam);
            Ok(out)
        }),
    )
}

#[must_use]
pub fn outlet_temperature<'g>(
    t_f: A<'g>,
    u_face: &[A<'g>; 3],
    flow_axis: usize,
    ports: Option<&PressurePorts>,
) -> A<'g> {
    let g = t_f.graph();
    let s = t_f.shape().as3();
    if let Some(pp) = ports {
        let a = pp.axis;
        let last = pp.side == 1;
        let f = plane(u_face[a], a, last);
        let (_, om) = pp.cell_masks(s);
        let o = plane(g.constant(om, Shape::d3(s)), a, last);
        let f_out = if pp.side == 0 { -f * o } else { f * o };
        let num = (f_out * plane(t_f, a, last)).sum();
        return num / (f_out.sum() + TINY);
    }
    let a = flow_axis;
    let f_out = plane(u_face[a], a, true);
    (f_out * plane(t_f, a, true)).sum() / (f_out.sum() + TINY)
}

#[must_use]
pub fn bulk_rise_reference(mdot: A<'_>, cp: f64, p_total: f64) -> A<'_> {
    let mf = super::darcy::MDOT_FLOOR;
    mdot.map(move |m| (m * m + mf * mf).sqrt().recip() * (p_total / cp))
}

#[must_use]
pub fn cell_peclet(u_s: &[f64], h: f64, coolant: &Coolant) -> (Vec<f64>, Vec<f64>, f64) {
    let alpha_f = coolant.k_f / (coolant.rho_f * coolant.cp);
    let alpha_num: Vec<f64> = u_s.iter().map(|u| u * h / 2.0).collect();
    (alpha_num.iter().map(|a| a / alpha_f * 2.0).collect(), alpha_num, alpha_f)
}

#[must_use]
pub fn setup(
    coolant: &Coolant,
    h: f64,
    flow_axis: Option<usize>,
    ports: Option<&PressurePorts>,
) -> FluidSetup {
    FluidSetup {
        c: coolant.rho_f * coolant.cp,
        t_in: coolant.t_in,
        k_f: coolant.k_f,
        h,
        flow_axis: flow_axis.unwrap_or(FLOW_AXIS_DEFAULT),
        ports: ports.cloned(),
    }
}
