// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use implexity_ad::{AdError, Dual, Scalar};

use super::ad::{A, Graph, Shape};
use super::grid::flat;
use super::ops::{box_blur3, central_grad, concat_axis, hi_part, lo_part, plane, slice_axis, sub};
use super::porous;
use super::stencil::CellPattern;

pub const N_PICARD_DEFAULT: usize = 4;
pub const N_PICARD_MASS_FLOW_DEFAULT: usize = 12;
pub const A_V_FLOOR_REL: f64 = 1.0;
pub const A_V_FLOOR_SMOOTH_REL: f64 = 0.001;
pub const K_FLOOR_REL: f64 = 1e-9;
pub const D_H_MIN_REL: f64 = 0.1;
pub const D_H_MAX_REL: f64 = 4.0;
pub const D_H_SMOOTH_REL: f64 = 0.05;
pub const EPS_FLOW_FLOOR: f64 = 1e-6;
pub const C_DOT_FLOOR: f64 = 1e-30;
pub const MDOT_FLOOR: f64 = 1e-12;
pub const TINY: f64 = 1e-300;
pub const ANISO_NEWTON_STEPS: usize = 48;
pub const FLOW_AXIS_DEFAULT: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coolant {
    pub rho_f: f64,
    pub mu: f64,
    pub k_f: f64,
    pub cp: f64,
    pub t_in: f64,
    pub pr: f64,
    pub dp_budget: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PressurePorts {
    pub axis: usize,
    pub side: usize,
    pub inlet: Vec<f64>,
    pub outlet: Vec<f64>,
}

impl PressurePorts {
    pub fn new(
        axis: usize,
        side: usize,
        inlet: Vec<f64>,
        outlet: Vec<f64>,
        shape: [usize; 3],
    ) -> Result<Self, String> {
        if axis > 2 {
            return Err(format!("pressure-port axis must be 0, 1 or 2, got {axis}"));
        }
        if side > 1 {
            return Err(format!("pressure-port side must be 0 (low) or 1 (high), got {side}"));
        }
        let plane: usize = (0..3).filter(|&i| i != axis).map(|i| shape[i]).product();
        if inlet.len() != outlet.len() || inlet.len() != plane {
            return Err("pressure-port masks must have equal 2-D boundary-plane shapes".into());
        }
        for (name, m) in [("inlet", &inlet), ("outlet", &outlet)] {
            if m.iter().any(|v| !v.is_finite() || (*v != 0.0 && (*v - 1.0).abs() > 0.0)) {
                return Err(format!("{name} pressure-port mask must be finite and binary"));
            }
            if !m.iter().any(|v| *v != 0.0) {
                return Err(format!("{name} pressure-port mask must select at least one face"));
            }
        }
        if inlet.iter().zip(&outlet).any(|(a, b)| *a != 0.0 && *b != 0.0) {
            return Err("inlet and outlet pressure-port masks must be disjoint".into());
        }
        Ok(Self { axis, side, inlet, outlet })
    }

    #[must_use]
    pub fn cell_masks(&self, s: [usize; 3]) -> (Vec<f64>, Vec<f64>) {
        let n = s[0] * s[1] * s[2];
        let (mut i_m, mut o_m) = (vec![0.0; n], vec![0.0; n]);
        let idx = if self.side == 0 { 0 } else { s[self.axis] - 1 };
        let mut q = 0usize;
        for i in 0..s[0] {
            for j in 0..s[1] {
                for k in 0..s[2] {
                    if [i, j, k][self.axis] == idx {
                        let c = flat(s, i, j, k);
                        i_m[c] = self.inlet[q];
                        o_m[c] = self.outlet[q];
                        q += 1;
                    }
                }
            }
        }
        (i_m, o_m)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Lam<'g> {
    pub axes: [A<'g>; 3],
    pub iso: bool,
}

impl<'g> Lam<'g> {
    #[must_use]
    pub fn iso(l: A<'g>) -> Self {
        Self { axes: [l; 3], iso: true }
    }

    #[must_use]
    pub fn arrays(&self) -> Vec<A<'g>> {
        if self.iso { vec![self.axes[0]] } else { self.axes.to_vec() }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PoreGeometry<'g> {
    pub a_v: A<'g>,
    pub eps: A<'g>,
    pub d_h: A<'g>,
    pub k: A<'g>,
    pub c_f: A<'g>,
}

#[must_use]
pub fn pore_geometry<'g>(
    rho: A<'g>,
    h: f64,
    smooth_len: Option<f64>,
    a_v: Option<A<'g>>,
    w_closure: Option<&[A<'g>; 7]>,
) -> PoreGeometry<'g> {
    let g = rho.graph();
    let eps = 1.0 - rho;
    let (coarea, a_floor) = if let Some(a) = a_v {
        (a, A_V_FLOOR_SMOOTH_REL / h)
    } else {
        let [gx, gy, gz] = central_grad(rho, h);
        let c = g.mapn([gx, gy, gz], |[x, y, z]| (x * x + y * y + z * z + 1e-30).sqrt());
        match smooth_len {
            None => (c, A_V_FLOOR_REL / h),
            Some(sl) => (box_blur3(c, 1usize.max(porous::py_round(sl / h))), A_V_FLOOR_SMOOTH_REL / h),
        }
    };
    let av = coarea.map(move |c| (c * c + a_floor * a_floor).sqrt());
    let band = smooth_len.map_or(h, |sl| h.max(sl));
    let d_h = g.mapn([eps, av], move |[e, a]| {
        porous::soft_band(e * 4.0 / a, D_H_MIN_REL * band, D_H_MAX_REL * band, D_H_SMOOTH_REL * band)
    });
    let (ck, cf) = match w_closure {
        None => (eps.map(porous::kozeny_gyroid), eps.map(|e| porous::forchheimer_gyroid(e, true))),
        Some(w) => {
            let ins = [eps, w[0], w[1], w[2], w[3], w[4], w[5], w[6]];
            let ck = g.mapn(ins, |x| porous::kozeny_mixed(x[0], &[x[1], x[2], x[3], x[4], x[5], x[6], x[7]]));
            let cf = g.mapn(ins, |x| {
                porous::forchheimer_mixed(x[0], &[x[1], x[2], x[3], x[4], x[5], x[6], x[7]], true)
            });
            (ck, cf)
        }
    };
    let kf = K_FLOOR_REL * h * h;
    let k = g.mapn([ck, eps, d_h], move |[c, e, d]| c * e.powi(3) * d * d / 16.0 + kf);
    PoreGeometry { a_v: av, eps, d_h, k, c_f: cf }
}

fn harm<'g>(a: A<'g>, b: A<'g>, h: f64) -> A<'g> {
    a.graph().mapn([a, b], move |[x, y]| x * y * (2.0 * h) / (x + y + TINY))
}

#[must_use]
pub fn faces<'g>(lam: &Lam<'g>, h: f64) -> [A<'g>; 3] {
    std::array::from_fn(|a| harm(lo_part(lam.axes[a], a), hi_part(lam.axes[a], a), h))
}

#[must_use]
pub fn boundary_weight(s: [usize; 3], flow_axis: usize, ports: Option<&PressurePorts>) -> Vec<f64> {
    if let Some(p) = ports {
        let (i, o) = p.cell_masks(s);
        return i.iter().zip(&o).map(|(a, b)| a + b).collect();
    }
    let mut w = vec![0.0; s[0] * s[1] * s[2]];
    for i in 0..s[0] {
        for j in 0..s[1] {
            for k in 0..s[2] {
                let c = [i, j, k][flow_axis];
                if c == 0 {
                    w[flat(s, i, j, k)] += 1.0;
                }
                if c == s[flow_axis] - 1 {
                    w[flat(s, i, j, k)] += 1.0;
                }
            }
        }
    }
    w
}

#[must_use]
pub fn apply_a<'g>(lam: &Lam<'g>, p: A<'g>, bw: &[f64], h: f64, flow_axis: usize) -> A<'g> {
    let g = p.graph();
    let s = p.shape().as3();
    let t = faces(lam, h);
    let mut out = g.full(0.0, Shape::d3(s));
    for a in 0..3 {
        let f = t[a] * (lo_part(p, a) - hi_part(p, a));
        let mut lo = [0; 3];
        lo[a] = 0;
        out = out + super::ops::embed(f, lo, s);
        let mut lo1 = [0; 3];
        lo1[a] = 1;
        out = out - super::ops::embed(f, lo1, s);
    }
    let bwc = g.constant(bw.to_vec(), Shape::d3(s));
    out + bwc * lam.axes[flow_axis] * p * (2.0 * h)
}

#[must_use]
pub fn assemble_a(pat: &CellPattern, lam: &[Vec<f64>; 3], bw: &[f64], h: f64, flow_axis: usize) -> Vec<f64> {
    let s = pat.shape;
    let mut data = pat.zeros();
    for a in 0..3 {
        for i in 0..s[0] {
            for j in 0..s[1] {
                for k in 0..s[2] {
                    let p = [i, j, k];
                    if p[a] + 1 >= s[a] {
                        continue;
                    }
                    let mut q = p;
                    q[a] += 1;
                    let (c0, c1) = (flat(s, i, j, k), flat(s, q[0], q[1], q[2]));
                    let (x, y) = (lam[a][c0], lam[a][c1]);
                    let t = 2.0 * h * x * y / (x + y + TINY);
                    data[pat.diag[c0]] += t;
                    data[pat.diag[c1]] += t;
                    data[pat.slot(c0, c1)] -= t;
                    data[pat.slot(c1, c0)] -= t;
                }
            }
        }
    }
    for c in 0..pat.n {
        data[pat.diag[c]] += 2.0 * h * bw[c] * lam[flow_axis][c];
    }
    data
}

fn lam_values(lam: &Lam<'_>) -> [Vec<f64>; 3] {
    if lam.iso {
        let v = lam.axes[0].val();
        [v.clone(), v.clone(), v]
    } else {
        std::array::from_fn(|a| lam.axes[a].val())
    }
}

#[must_use]
pub fn solve_pressure_lam<'g>(
    lam: &Lam<'g>,
    h: f64,
    dp: A<'g>,
    flow_axis: usize,
    ports: Option<&PressurePorts>,
) -> A<'g> {
    let g = dp.graph();
    let a = ports.map_or(flow_axis, |p| p.axis);
    let s = lam.axes[0].shape().as3();
    let n = s[0] * s[1] * s[2];
    let bw = boundary_weight(s, a, ports);
    let inlet: Vec<f64> = match ports {
        None => {
            let mut v = vec![0.0; n];
            for i in 0..s[0] {
                for j in 0..s[1] {
                    for k in 0..s[2] {
                        if [i, j, k][a] == 0 {
                            v[flat(s, i, j, k)] += 1.0;
                        }
                    }
                }
            }
            v
        }
        Some(p) => p.cell_masks(s).0,
    };
    let inl = g.constant(inlet, Shape::d3(s));
    let b = lam.axes[a] * inl * dp * (2.0 * h);
    let pat = CellPattern::of(s);
    let lv = lam_values(lam);
    let data = assemble_a(&pat, &lv, &bw, h, a);
    let fact = match pat.cholesky(data.clone()) {
        Ok(f) => Arc::new(f),
        Err(e) => return g.failed(&format!("pressure solve: {e}"), Shape::d3(s)),
    };
    let bv = b.val();
    let mut x = bv.clone();
    if let Err(e) = fact.solve_in_place(&mut x) {
        return g.failed(&format!("pressure solve: {e}"), Shape::d3(s));
    }
    let ax = pat.matvec(&data, &x);
    let x = checked(x, &ax, &bv);
    let xs = Arc::new(x.clone());
    let lv = Arc::new(lv);
    let iso = lam.iso;
    let bw = Arc::new(bw);
    let mut inputs = lam.arrays();
    inputs.push(b);
    g.custom(
        &inputs,
        x,
        Shape::d3(s),
        Box::new(move |gx: &[f64]| {
            let mut lamb = gx.to_vec();
            fact.solve_in_place(&mut lamb).map_err(|e| AdError::Callback(e.to_string()))?;
            let sub = Graph::new();
            let axes: Vec<A<'_>> = if iso {
                vec![sub.input(lv[0].clone(), Shape::d3(s))]
            } else {
                (0..3).map(|k| sub.input(lv[k].clone(), Shape::d3(s))).collect()
            };
            let l =
                if iso { Lam::iso(axes[0]) } else { Lam { axes: [axes[0], axes[1], axes[2]], iso: false } };
            let pc = sub.constant(xs.as_ref().clone(), Shape::d3(s));
            let y = apply_a(&l, pc, &bw, h, a);
            let gr = sub.vjp(y, &lamb, &axes).map_err(AdError::Callback)?;
            let mut out: Vec<Vec<f64>> =
                gr.into_iter().map(|v| v.into_iter().map(|x| -x).collect()).collect();
            out.push(lamb);
            Ok(out)
        }),
    )
}

pub(crate) fn checked(x: Vec<f64>, ax: &[f64], b: &[f64]) -> Vec<f64> {
    let nb = b.iter().map(|v| v * v).sum::<f64>().sqrt();
    let nr = b.iter().zip(ax).map(|(p, q)| (p - q) * (p - q)).sum::<f64>().sqrt();
    let rel = nr / if nb > 0.0 { nb } else { 1.0 };
    if x.iter().all(Scalar::is_finite) && rel <= 1e-8 { x } else { vec![f64::NAN; x.len()] }
}

#[must_use]
pub fn outlet_flow_lam<'g>(
    lam: &Lam<'g>,
    h: f64,
    p: A<'g>,
    flow_axis: usize,
    ports: Option<&PressurePorts>,
) -> A<'g> {
    let g = p.graph();
    let s = p.shape().as3();
    if let Some(pp) = ports {
        let a = pp.axis;
        let last = pp.side == 1;
        let lam_n = plane(lam.axes[a], a, last);
        let pl = plane(p, a, last);
        let (_, o) = pp.cell_masks(s);
        let om = plane(g.constant(o, Shape::d3(s)), a, last);
        return (lam_n * om * pl * (2.0 * h)).sum();
    }
    let a = flow_axis;
    (plane(lam.axes[a], a, true) * plane(p, a, true) * (2.0 * h)).sum()
}

#[must_use]
pub fn velocity_lam<'g>(
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
    let a2 = h * h;
    let q: [A<'g>; 3] = std::array::from_fn(|ax| t[ax] * (lo_part(p, ax) - hi_part(p, ax)) / a2);
    let port_flux = ports.map(|pp| {
        let (ax, side, f) = port_face_flux(lam, h, p, dp, pp);
        (ax, side, f / a2)
    });
    std::array::from_fn(|ax| {
        let mut sh = s;
        sh[ax] = 1;
        let zero = || g.full(0.0, Shape::d3(sh));
        let (f_lo, f_hi) = match &port_flux {
            Some((pax, pside, f)) => {
                if *pax == ax {
                    if *pside == 0 { (*f, zero()) } else { (zero(), *f) }
                } else {
                    (zero(), zero())
                }
            }
            None => {
                if ax == flow_axis {
                    let lo = plane(lam.axes[ax], ax, false);
                    let hi = plane(lam.axes[ax], ax, true);
                    let flo = lo * 2.0 * (dp - plane(p, ax, false)) / h;
                    let fhi = hi * 2.0 * plane(p, ax, true) / h;
                    (flo, fhi)
                } else {
                    (zero(), zero())
                }
            }
        };
        let full = concat_axis(&[f_lo, q[ax], f_hi], ax);
        (lo_part(full, ax) + hi_part(full, ax)) * 0.5
    })
}

#[must_use]
pub fn port_face_flux<'g>(
    lam: &Lam<'g>,
    h: f64,
    p: A<'g>,
    dp: A<'g>,
    pp: &PressurePorts,
) -> (usize, usize, A<'g>) {
    let g = p.graph();
    let s = p.shape().as3();
    let a = pp.axis;
    let last = pp.side == 1;
    let (im, om) = pp.cell_masks(s);
    let inl = plane(g.constant(im, Shape::d3(s)), a, last);
    let out = plane(g.constant(om, Shape::d3(s)), a, last);
    let lam_n = plane(lam.axes[a], a, last);
    let pc = plane(p, a, last);
    let inward = lam_n * (inl * dp - (inl + out) * pc) * (2.0 * h);
    let oriented = if pp.side == 0 { inward } else { -inward };
    (a, pp.side, oriented)
}

#[must_use]
pub fn speed<'g>(u: &[A<'g>; 3]) -> A<'g> {
    u[0].graph().mapn([u[0], u[1], u[2]], |[x, y, z]| (x * x + y * y + z * z + TINY).sqrt())
}

#[must_use]
pub fn anisotropic_mobility<'g>(
    ga: [A<'g>; 3],
    ka: [A<'g>; 3],
    c_f: A<'g>,
    rho_f: f64,
    mu: f64,
) -> [A<'g>; 3] {
    let gr = c_f.graph();
    gr.mapn_multi([ga[0], ga[1], ga[2], ka[0], ka[1], ka[2], c_f], move |x: [Dual<7>; 7]| {
        aniso_cell(&[x[0], x[1], x[2]], &[x[3], x[4], x[5]], x[6], rho_f, mu)
    })
}

fn aniso_cell<S: Scalar>(g: &[S; 3], k: &[S; 3], cf: S, rho_f: f64, mu: f64) -> [S; 3] {
    let a: [S; 3] = std::array::from_fn(|i| S::from_f64(mu) / (k[i] + TINY));
    let b: [S; 3] = std::array::from_fn(|i| cf * rho_f / (k[i] + TINY).sqrt());
    let gmag = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2] + TINY).sqrt();
    let abar = (a[0] + a[1] + a[2]) / 3.0;
    let bbar = (b[0] + b[1] + b[2]) / 3.0;
    let mut s = gmag * 2.0 / (abar + (abar * abar + bbar * gmag * 4.0).sqrt() + TINY);
    let mut lower = S::zero();
    let mut sum = S::zero();
    for i in 0..3 {
        let r = g[i] / (a[i] + TINY);
        sum += r * r;
    }
    let mut upper = (sum + TINY).sqrt();
    s = s.minimum(upper);
    for _ in 0..ANISO_NEWTON_STEPS {
        let d: [S; 3] = std::array::from_fn(|i| a[i] + b[i] * s);
        let u: [S; 3] = std::array::from_fn(|i| g[i] / (d[i] + TINY));
        let f = s * s - (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]);
        let mut acc = S::zero();
        for i in 0..3 {
            acc += u[i] * u[i] * b[i] / (d[i] + TINY);
        }
        let df = s * 2.0 + acc * 2.0;
        if f.value() < 0.0 {
            lower = s;
        }
        if f.value() > 0.0 {
            upper = s;
        }
        let cand = s - f / (df + TINY);
        s = if cand.value() >= lower.value() && cand.value() <= upper.value() {
            cand
        } else {
            (lower + upper) * 0.5
        };
    }
    std::array::from_fn(|i| S::from_f64(1.0) / (a[i] + b[i] * s + TINY))
}

#[derive(Clone, Copy, Debug)]
pub enum KSolve<'g> {
    Iso(A<'g>),
    Axes([A<'g>; 3]),
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn solve_darcy_forchheimer<'g>(
    k: KSolve<'g>,
    c_f: A<'g>,
    coolant: &Coolant,
    h: f64,
    dp: A<'g>,
    n_picard: usize,
    flow_axis: usize,
    ports: Option<&PressurePorts>,
) -> (A<'g>, Lam<'g>) {
    let mu = coolant.mu;
    match k {
        KSolve::Axes(ka) => {
            let mut lam = Lam { axes: std::array::from_fn(|i| ka[i] / mu), iso: false };
            let mut p = solve_pressure_lam(&lam, h, dp, flow_axis, ports);
            for _ in 0..n_picard {
                let u = velocity_lam(&lam, h, p, dp, flow_axis, ports);
                let g: [A<'g>; 3] = std::array::from_fn(|i| u[i] / (lam.axes[i] + TINY));
                lam = Lam { axes: anisotropic_mobility(g, ka, c_f, coolant.rho_f, mu), iso: false };
                p = solve_pressure_lam(&lam, h, dp, flow_axis, ports);
            }
            (p, lam)
        }
        KSolve::Iso(kk) => {
            let mut lam = Lam::iso(kk / mu);
            let mut p = solve_pressure_lam(&lam, h, dp, flow_axis, ports);
            let rho_f = coolant.rho_f;
            for _ in 0..n_picard {
                let u = velocity_lam(&lam, h, p, dp, flow_axis, ports);
                let us = speed(&u);
                let g = us / lam.axes[0];
                lam = Lam::iso(
                    c_f.graph()
                        .mapn([g, kk, c_f], move |[g, k, c]| porous::effective_mobility(g, k, c, rho_f, mu)),
                );
                p = solve_pressure_lam(&lam, h, dp, flow_axis, ports);
            }
            (p, lam)
        }
    }
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn solve_darcy_forchheimer_mdot<'g>(
    k: KSolve<'g>,
    c_f: A<'g>,
    coolant: &Coolant,
    h: f64,
    mass_flow_kg_s: f64,
    n_picard: usize,
    flow_axis: usize,
    ports: Option<&PressurePorts>,
) -> (A<'g>, Lam<'g>, A<'g>) {
    let g = c_f.graph();
    let mu = coolant.mu;
    let target_q = mass_flow_kg_s / coolant.rho_f;
    let normalize = |lam: &Lam<'g>| -> (A<'g>, A<'g>) {
        let unit = g.scalar(1.0);
        let p_unit = solve_pressure_lam(lam, h, unit, flow_axis, ports);
        let q_unit = outlet_flow_lam(lam, h, p_unit, flow_axis, ports);
        let valid =
            q_unit.item().is_finite() && q_unit.item() > 0.0 && target_q.is_finite() && target_q > 0.0;
        let drop = if valid { q_unit.map(move |q| Dual::constant(target_q) / q) } else { g.scalar(f64::NAN) };
        (drop * p_unit, drop)
    };
    match k {
        KSolve::Axes(ka) => {
            let mut lam = Lam { axes: std::array::from_fn(|i| ka[i] / mu), iso: false };
            let (mut p, mut drop) = normalize(&lam);
            for _ in 0..n_picard {
                let u = velocity_lam(&lam, h, p, drop, flow_axis, ports);
                let gg: [A<'g>; 3] = std::array::from_fn(|i| u[i] / (lam.axes[i] + TINY));
                lam = Lam { axes: anisotropic_mobility(gg, ka, c_f, coolant.rho_f, mu), iso: false };
                (p, drop) = normalize(&lam);
            }
            (p, lam, drop)
        }
        KSolve::Iso(kk) => {
            let mut lam = Lam::iso(kk / mu);
            let (mut p, mut drop) = normalize(&lam);
            let rho_f = coolant.rho_f;
            for _ in 0..n_picard {
                let u = velocity_lam(&lam, h, p, drop, flow_axis, ports);
                let us = speed(&u);
                let gg = us / (lam.axes[0] + TINY);
                lam =
                    Lam::iso(g.mapn([gg, kk, c_f], move |[g, k, c]| {
                        porous::effective_mobility(g, k, c, rho_f, mu)
                    }));
                (p, drop) = normalize(&lam);
            }
            (p, lam, drop)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CoolantFields<'g> {
    pub p: A<'g>,
    pub pressure_drop: A<'g>,
    pub u_s: A<'g>,
    pub u: A<'g>,
    pub vel: [A<'g>; 3],
    pub re: A<'g>,
    pub re_pore: A<'g>,
    pub nu: A<'g>,
    pub h_conv: A<'g>,
    pub a_wet: A<'g>,
    pub g_wall: A<'g>,
    pub g: A<'g>,
    pub q: A<'g>,
    pub mdot: A<'g>,
    pub p_pump: A<'g>,
    pub d_h: A<'g>,
    pub k: A<'g>,
    pub k_axes: Option<[A<'g>; 3]>,
    pub eps: A<'g>,
    pub a_v: A<'g>,
    pub c_f: A<'g>,
    pub fo: A<'g>,
    pub lam: Lam<'g>,
    pub n_picard: usize,
    pub flow_axis: usize,
    pub rel_residual: f64,
    pub residual_norm: f64,
}

#[derive(Clone, Debug)]
pub struct CoolantOptions<'a> {
    pub smooth_len: Option<f64>,
    pub forchheimer: bool,
    pub n_picard: usize,
    pub flow_axis: usize,
    pub nusselt_law: Option<String>,
    pub ports: Option<&'a PressurePorts>,
    pub mass_flow_kg_s: Option<f64>,
    pub mass_flow_n_picard: usize,
}

impl Default for CoolantOptions<'_> {
    fn default() -> Self {
        Self {
            smooth_len: None,
            forchheimer: true,
            n_picard: N_PICARD_DEFAULT,
            flow_axis: FLOW_AXIS_DEFAULT,
            nusselt_law: None,
            ports: None,
            mass_flow_kg_s: None,
            mass_flow_n_picard: N_PICARD_MASS_FLOW_DEFAULT,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Directional<'g> {
    KAxes([A<'g>; 3]),
    Shape([A<'g>; 3]),
}

pub fn coolant_fields<'g>(
    rho: A<'g>,
    h: f64,
    coolant: &Coolant,
    a_v: Option<A<'g>>,
    w_closure: Option<&[A<'g>; 7]>,
    directional: Option<Directional<'g>>,
    opts: &CoolantOptions<'_>,
) -> Result<CoolantFields<'g>, String> {
    let g = rho.graph();
    let geo = pore_geometry(rho, h, opts.smooth_len, a_v, w_closure);
    let fa = opts.flow_axis;
    let k_solve = match directional {
        Some(Directional::KAxes(ka)) => KSolve::Axes(ka),
        Some(Directional::Shape(sh)) => KSolve::Axes(std::array::from_fn(|i| geo.k * sh[i])),
        None => KSolve::Iso(geo.k),
    };
    let (p, lam, drop, n_pic) = match opts.mass_flow_kg_s {
        None => {
            let n = if opts.forchheimer { opts.n_picard } else { 0 };
            let dp = g.scalar(coolant.dp_budget);
            let (p, lam) = solve_darcy_forchheimer(k_solve, geo.c_f, coolant, h, dp, n, fa, opts.ports);
            (p, lam, dp, n)
        }
        Some(m) => {
            let n = if opts.forchheimer { opts.mass_flow_n_picard } else { 0 };
            let (p, lam, drop) =
                solve_darcy_forchheimer_mdot(k_solve, geo.c_f, coolant, h, m, n, fa, opts.ports);
            (p, lam, drop, n)
        }
    };
    let vel = velocity_lam(&lam, h, p, drop, fa, opts.ports);
    let u_s = speed(&vel);
    let u = u_s / (geo.eps + EPS_FLOW_FLOOR);
    let (rf, mu, pr, kf, cp) = (coolant.rho_f, coolant.mu, coolant.pr, coolant.k_f, coolant.cp);
    let re = g.mapn([u_s, geo.d_h], move |[v, d]| v * rf * d / mu);
    let re_pore = g.mapn([u, geo.d_h], move |[v, d]| v * rf * d / mu);
    let law = opts.nusselt_law.clone();
    if let Some(l) = &law
        && !porous::NUSSELT_LAWS.contains(&l.as_str())
    {
        return Err(format!("unknown nusselt law '{l}'; known: {}", porous::NUSSELT_LAWS.join(", ")));
    }
    let law2 = law.clone();
    let nu = g.mapn([re, geo.eps], move |[r, e]| {
        porous::nusselt(r, pr, true, law2.as_deref(), e).unwrap_or_else(|_| Dual::constant(f64::NAN))
    });
    let h_conv = g.mapn([nu, geo.d_h], move |[n, d]| n * kf / d);
    let a_wet = geo.a_v * h.powi(3);
    let g_wall = h_conv * a_wet;
    let c_dot = u_s.map(move |v| v * (rf * cp) * (h * h) + C_DOT_FLOOR);
    let gg = g.mapn([c_dot, g_wall], |[c, w]| c * (-(-(w / c)).exp_m1()));
    let fo = g.mapn([geo.k, geo.c_f, u_s], move |[k, c, v]| porous::forchheimer_number(k, c, v, rf, mu));
    let q = outlet_flow_lam(&lam, h, p, fa, opts.ports);
    let mdot = q * rf;
    let p_pump = drop * q;
    let (rel, norm) =
        momentum_residual(&vel, &lam, &k_solve, geo.c_f, coolant, opts.forchheimer && n_pic > 0);
    Ok(CoolantFields {
        p,
        pressure_drop: drop,
        u_s,
        u,
        vel,
        re,
        re_pore,
        nu,
        h_conv,
        a_wet,
        g_wall,
        g: gg,
        q,
        mdot,
        p_pump,
        d_h: geo.d_h,
        k: geo.k,
        k_axes: match k_solve {
            KSolve::Axes(a) => Some(a),
            KSolve::Iso(_) => None,
        },
        eps: geo.eps,
        a_v: geo.a_v,
        c_f: geo.c_f,
        fo,
        lam,
        n_picard: n_pic,
        flow_axis: fa,
        rel_residual: rel,
        residual_norm: norm,
    })
}

#[must_use]
pub fn momentum_residual(
    vel: &[A<'_>; 3],
    lam: &Lam<'_>,
    k: &KSolve<'_>,
    c_f: A<'_>,
    coolant: &Coolant,
    forchheimer: bool,
) -> (f64, f64) {
    let v: [Vec<f64>; 3] = std::array::from_fn(|i| vel[i].val());
    let l: [Vec<f64>; 3] = std::array::from_fn(|i| lam.axes[i].val());
    let kk: [Vec<f64>; 3] = match k {
        KSolve::Iso(a) => {
            let x = a.val();
            [x.clone(), x.clone(), x]
        }
        KSolve::Axes(a) => std::array::from_fn(|i| a[i].val()),
    };
    let cf = c_f.val();
    let n = v[0].len();
    let (mut gn, mut rn) = (0.0f64, 0.0f64);
    let mut gsum = [0.0f64; 3];
    let mut rsum = [0.0f64; 3];
    for c in 0..n {
        let sp = (v[0][c] * v[0][c] + v[1][c] * v[1][c] + v[2][c] * v[2][c] + TINY).sqrt();
        for i in 0..3 {
            let grad = v[i][c] / l[i][c];
            let coef = coolant.mu / kk[i][c]
                + if forchheimer { coolant.rho_f * cf[c] * sp / kk[i][c].sqrt() } else { 0.0 };
            let r = grad - coef * v[i][c];
            gsum[i] += grad * grad;
            rsum[i] += r * r;
        }
    }
    for i in 0..3 {
        gn += gsum[i];
        rn += rsum[i];
    }
    let (gn, rn) = (gn.sqrt(), rn.sqrt());
    (rn / if gn > 0.0 { gn } else { 1.0 }, rn)
}

#[must_use]
pub fn bulk_temperature(mdot: f64, cp: f64, t_in: f64, p_tot: f64, s: [usize; 3], h: f64) -> Vec<f64> {
    let lz = s[2] as f64 * h;
    let ms = (mdot * mdot + MDOT_FLOOR * MDOT_FLOOR).sqrt();
    let mut out = Vec::with_capacity(s[0] * s[1] * s[2]);
    for _ in 0..s[0] {
        for _ in 0..s[1] {
            for k in 0..s[2] {
                let z = (k as f64 + 0.5) * h;
                out.push(t_in + p_tot * (z / lz) / (ms * cp));
            }
        }
    }
    out
}

#[must_use]
pub fn sub_box(a: A<'_>, lo: [usize; 3], hi: [usize; 3]) -> A<'_> {
    sub(a, lo, hi)
}

#[must_use]
pub fn slice(a: A<'_>, axis: usize, start: usize, end: usize) -> A<'_> {
    slice_axis(a, axis, start, end)
}
