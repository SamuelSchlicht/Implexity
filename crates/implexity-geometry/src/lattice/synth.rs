// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use rayon::prelude::*;

use crate::error::{GResult, GeometryError};
use crate::lattice::config::{Channels, LatticeConfig};
use crate::lattice::controls::{CHANNEL_SLICES, Controls};
use crate::lattice::numerics::{
    Grids, Interp, box_blur3, box_blur3_t, cumintegrate, cumintegrate_t, grad_center, grad_center_t,
};
use crate::scalar::{RevTape, Rv, Scalar};

pub const N_BASIS: usize = 8;

#[must_use]
pub fn basis_rms() -> [f64; N_BASIS] {
    [
        1.5f64.sqrt(),
        0.75f64.sqrt(),
        0.125f64.sqrt(),
        0.125f64.sqrt(),
        0.75f64.sqrt(),
        1.5f64.sqrt(),
        0.75f64.sqrt(),
        0.375f64.sqrt(),
    ]
}

pub const FAMILY_RAW: [(&str, [f64; N_BASIS]); 7] = [
    ("schwarz_p", [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("gyroid", [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("diamond", [0.0, 0.0, 1.0, -1.0, 0.0, 0.0, 0.0, 0.0]),
    ("iwp", [0.0, 0.0, 0.0, 0.0, 2.0, -1.0, 0.0, 0.0]),
    ("neovius", [3.0, 0.0, 4.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("frd", [0.0, 0.0, 4.0, 0.0, 0.0, 0.0, -1.0, 0.0]),
    ("lidinoid", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.5, 0.5]),
];

#[must_use]
pub fn family_names() -> Vec<&'static str> {
    FAMILY_RAW.iter().map(|(n, _)| *n).collect()
}

pub const GYROID: usize = 1;


pub fn family_vector(name: &str) -> GResult<[f64; N_BASIS]> {
    let raw = FAMILY_RAW.iter().find(|(n, _)| *n == name).map(|(_, r)| *r).ok_or_else(|| {
        GeometryError::Value(format!("unknown lattice family {}", crate::pyfmt::str_repr(name)))
    })?;
    let rms = basis_rms();
    let v: [f64; N_BASIS] = std::array::from_fn(|i| raw[i] * rms[i]);
    let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    Ok(v.map(|x| x / norm))
}

#[must_use]
pub fn family_matrix() -> Vec<[f64; N_BASIS]> {
    FAMILY_RAW.iter().map(|(n, _)| family_vector(n).unwrap_or([0.0; N_BASIS])).collect()
}

pub const FAMILY_SHARPNESS: f64 = 35.0;

pub const BASIS_PHASE_ORDER2: [f64; N_BASIS] = [1., 2., 3., 3., 2., 4., 8., 6.];
pub const BASIS_PHASE_ENERGY: [f64; N_BASIS] = [1., 2., 3., 3., 2., 1., 2., 3.];
pub const PHASE_ORDER_EPS: f64 = 1e-12;
pub const INTERFACE_FLOOR_ELEMS: f64 = 0.5;
pub const FLOOR_WIDTH_REL: f64 = 0.25;
pub const ALL_CHANNELS: [&str; 9] = ["a", "m", "dphi", "s", "w", "nu", "w2", "res", "c"];
pub const VOLUME_CHANNELS: [&str; 2] = ["a", "m"];

pub fn basis_and_grad<S: Scalar>(phi: [S; 3]) -> ([S; N_BASIS], [[S; N_BASIS]; 3]) {
    let [p1, p2, p3] = phi;
    let (c1, c2, c3) = (p1.cos(), p2.cos(), p3.cos());
    let (s1, s2, s3) = (p1.sin(), p2.sin(), p3.sin());
    let (bc1, bc2, bc3) = ((p1 * 2.0).cos(), (p2 * 2.0).cos(), (p3 * 2.0).cos());
    let (bs1, bs2, bs3) = ((p1 * 2.0).sin(), (p2 * 2.0).sin(), (p3 * 2.0).sin());
    let raw = [
        c1 + c2 + c3,
        s1 * c2 + s2 * c3 + s3 * c1,
        c1 * c2 * c3,
        s1 * s2 * s3,
        c1 * c2 + c2 * c3 + c3 * c1,
        bc1 + bc2 + bc3,
        bc1 * bc2 + bc2 * bc3 + bc3 * bc1,
        bs1 * c2 * s3 + bs2 * c3 * s1 + bs3 * c1 * s2,
    ];
    let d1 = [
        -s1,
        c1 * c2 - s3 * s1,
        -s1 * c2 * c3,
        c1 * s2 * s3,
        -s1 * (c2 + c3),
        bs1 * -2.0,
        bs1 * -2.0 * (bc2 + bc3),
        bc1 * 2.0 * c2 * s3 + bs2 * c3 * c1 - bs3 * s1 * s2,
    ];
    let d2 = [
        -s2,
        -s1 * s2 + c2 * c3,
        -c1 * s2 * c3,
        s1 * c2 * s3,
        -s2 * (c1 + c3),
        bs2 * -2.0,
        bs2 * -2.0 * (bc1 + bc3),
        -bs1 * s2 * s3 + bc2 * 2.0 * c3 * s1 + bs3 * c1 * c2,
    ];
    let d3 = [
        -s3,
        -s2 * s3 + c3 * c1,
        -c1 * c2 * s3,
        s1 * s2 * c3,
        -s3 * (c1 + c2),
        bs3 * -2.0,
        bs3 * -2.0 * (bc1 + bc2),
        bs1 * c2 * c3 - bs2 * s3 * s1 + bc3 * 2.0 * c1 * s2,
    ];
    let rms = basis_rms();
    let vals = std::array::from_fn(|i| raw[i] / rms[i]);
    let grads = [
        std::array::from_fn(|i| d1[i] / rms[i]),
        std::array::from_fn(|i| d2[i] / rms[i]),
        std::array::from_fn(|i| d3[i] / rms[i]),
    ];
    (vals, grads)
}


pub fn basis<S: Scalar>(phi: [S; 3]) -> [S; N_BASIS] {
    let [p1, p2, p3] = phi;
    let (c1, c2, c3) = (p1.cos(), p2.cos(), p3.cos());
    let (s1, s2, s3) = (p1.sin(), p2.sin(), p3.sin());
    let (bc1, bc2, bc3) = ((p1 * 2.0).cos(), (p2 * 2.0).cos(), (p3 * 2.0).cos());
    let (bs1, bs2, bs3) = ((p1 * 2.0).sin(), (p2 * 2.0).sin(), (p3 * 2.0).sin());
    let raw = [
        c1 + c2 + c3,
        s1 * c2 + s2 * c3 + s3 * c1,
        c1 * c2 * c3,
        s1 * s2 * s3,
        c1 * c2 + c2 * c3 + c3 * c1,
        bc1 + bc2 + bc3,
        bc1 * bc2 + bc2 * bc3 + bc3 * bc1,
        bs1 * c2 * s3 + bs2 * c3 * s1 + bs3 * c1 * s2,
    ];
    let rms = basis_rms();
    std::array::from_fn(|i| raw[i] / rms[i])
}

#[must_use]
pub fn phase_trig(p: f64) -> [f64; 4] {
    [p.cos(), p.sin(), (p * 2.0).cos(), (p * 2.0).sin()]
}

#[must_use]
pub fn basis_from_trig(t: &[[f64; 4]; 3]) -> [f64; N_BASIS] {
    let [[c1, s1, bc1, bs1], [c2, s2, bc2, bs2], [c3, s3, bc3, bs3]] = *t;
    let raw = [
        c1 + c2 + c3,
        s1 * c2 + s2 * c3 + s3 * c1,
        c1 * c2 * c3,
        s1 * s2 * s3,
        c1 * c2 + c2 * c3 + c3 * c1,
        bc1 + bc2 + bc3,
        bc1 * bc2 + bc2 * bc3 + bc3 * bc1,
        bs1 * c2 * s3 + bs2 * c3 * s1 + bs3 * c1 * s2,
    ];
    let rms = basis_rms();
    std::array::from_fn(|i| raw[i] / rms[i])
}

pub fn normalise_coefficients<S: Scalar>(w: [S; N_BASIS], eps: f64) -> [S; N_BASIS] {
    let mut ss = w[0] * w[0];
    for x in &w[1..] {
        ss = ss + *x * *x;
    }
    let norm = (ss + eps * eps).sqrt();
    w.map(|x| x / norm)
}

pub fn gauge_normalise(controls: &mut Controls) {
    let n = controls.n();
    let mut acc = 0.0;
    for c in 0..n {
        let mut s = 0.0;
        for i in 0..N_BASIS {
            let v = controls.data[(8 + i) * n + c];
            s += v * v;
        }
        acc += s;
    }
    #[allow(clippy::cast_precision_loss)]
    let norm = (acc / n as f64 + 1e-30).sqrt();
    for v in &mut controls.data[8 * n..16 * n] {
        *v /= norm;
    }
}

#[must_use]
pub fn family_weights(w_hat: &[f64; N_BASIS], sharpness: f64) -> Vec<f64> {
    let proj: Vec<f64> = family_matrix()
        .iter()
        .map(|row| row.iter().zip(w_hat).map(|(a, b)| a * b).sum::<f64>() * sharpness)
        .collect();
    let m = proj.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let e: Vec<f64> = proj.iter().map(|p| (p - m).exp()).collect();
    let s: f64 = e.iter().sum();
    e.iter().map(|x| x / s).collect()
}

#[must_use]
pub fn secondary_coincidences(ratio: f64) -> Vec<(usize, usize)> {
    if (ratio - 1.0).abs() == 0.0 {
        (0..N_BASIS).map(|i| (i, i)).collect()
    } else if (ratio - 2.0).abs() == 0.0 {
        vec![(5, 0), (6, 4)]
    } else if (ratio - 0.5).abs() == 0.0 {
        vec![(0, 5), (4, 6)]
    } else {
        Vec::new()
    }
}

pub fn phase_harmonic_order<S: Scalar>(w_hat: &[S; N_BASIS], sec_amp: Option<S>, lat: &LatticeConfig) -> S {
    let mut num = w_hat[0] * w_hat[0] * BASIS_PHASE_ORDER2[0];
    let mut den = w_hat[0] * w_hat[0] * BASIS_PHASE_ENERGY[0];
    for i in 1..N_BASIS {
        let w2 = w_hat[i] * w_hat[i];
        num = num + w2 * BASIS_PHASE_ORDER2[i];
        den = den + w2 * BASIS_PHASE_ENERGY[i];
    }
    if let Some(amp) = sec_amp {
        let r = lat.sec_ratio;
        let mut cn = S::cst(0.0);
        let mut cd = S::cst(0.0);
        for (i, j) in secondary_coincidences(r) {
            let cij = w_hat[i] * w_hat[j];
            cn = cn + cij * BASIS_PHASE_ORDER2[i];
            cd = cd + cij * BASIS_PHASE_ENERGY[i];
        }
        let a2 = amp * amp;
        num = num * (a2 * (r * r) + 1.0) + amp * 2.0 * cn;
        den = den * (a2 + 1.0) + amp * 2.0 * cd;
    }
    ((num + PHASE_ORDER_EPS) / (den + PHASE_ORDER_EPS)).sqrt()
}

#[must_use]
pub fn interface_elems(
    interface_w: f64,
    h: f64,
    interface_len: Option<f64>,
    floor_elems: f64,
) -> (f64, bool) {
    match interface_len {
        None => (interface_w, false),
        Some(len) => {
            let w = len / h;
            if w < floor_elems { (floor_elems, true) } else { (w, false) }
        }
    }
}

#[must_use]
pub fn resolve_interface_length(
    interface_w: f64,
    h: f64,
    interface_len: Option<f64>,
    floor_elems: f64,
) -> (f64, bool) {
    let (w, floored) = interface_elems(interface_w, h, interface_len, floor_elems);
    (w * h, floored)
}

pub fn soft_floor<S: Scalar>(x: S, floor: f64, width: f64) -> S {
    ((x - floor) / width).softplus() * width + floor
}

#[must_use]
pub fn describe_channels(channels: &Channels, control_grid: [usize; 3]) -> String {
    let n = control_grid[0] * control_grid[1] * control_grid[2];
    let mut rows: Vec<(&str, usize)> = channels
        .active()
        .into_iter()
        .map(|name| {
            let (_, a, b) = CHANNEL_SLICES.iter().find(|(k, _, _)| *k == name).copied().unwrap_or(("", 0, 1));
            (name, (b - a) * n)
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let total: usize = rows.iter().map(|r| r.1).sum();
    let parts: Vec<String> = rows.iter().map(|(k, v)| format!("{k} {v}")).collect();
    format!("{} channels, {total} design variables ({})", rows.len(), parts.join(", "))
}

#[must_use]
pub fn init_params(control_grid: [usize; 3], channels: &Channels) -> Controls {
    let n = control_grid[0] * control_grid[1] * control_grid[2];
    let mut data = vec![0.0; 20 * n];
    if channels.family {
        for v in &mut data[(8 + GYROID) * n..(9 + GYROID) * n] {
            *v = 1.0;
        }
    }
    Controls { grid: control_grid, data }
}

#[must_use]
pub fn shift_logits(controls: &Controls, s: f64) -> Controls {
    let n = controls.n();
    let mut out = controls.clone();
    for v in &mut out.data[..2 * n] {
        *v += s;
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct SynthOptions {
    pub interface_w: f64,
    pub beta_mask: f64,
    pub t_offset: f64,
    pub beta_mat: f64,
    pub beta_topo: f64,
    pub channels: Channels,
    pub interface_len: Option<f64>,
}

impl Default for SynthOptions {
    fn default() -> Self {
        Self {
            interface_w: 1.0,
            beta_mask: 8.0,
            t_offset: 0.0,
            beta_mat: 8.0,
            beta_topo: 8.0,
            channels: Channels::default(),
            interface_len: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Fields {
    pub rho: Vec<f64>,
    pub rho_lat: Vec<f64>,
    pub mtilde: Vec<f64>,
    pub phase_fraction: Vec<f64>,
    pub interface_len: f64,
    pub interface_elems: f64,
    pub interface_floored: bool,
    pub f: Vec<f64>,
    pub d: Vec<f64>,
    pub q: Vec<f64>,
    pub g: Vec<f64>,
    pub t: Vec<f64>,
    pub tau: Vec<f64>,
    pub nu: Vec<f64>,
    pub gnorm: Vec<f64>,
    pub geff: Vec<f64>,
    pub grad_d: Vec<f64>,
    pub grad_q: Vec<f64>,
    pub phi: [Vec<f64>; 3],
    pub w_hat: Vec<Vec<f64>>,
    pub fold_weight: Vec<f64>,
    pub jac: [[Vec<f64>; 3]; 3],
    pub pen_det: f64,
    pub pen_band: f64,
    pub pen_cond: f64,
    pub det_rel_min: f64,
    pub cond_max: f64,
    pub knorm_over_cap: f64,
    pub band_over_fraction: f64,
    pub band_min_elems_per_period: f64,
    pub band_phase_order_max: f64,
    pub band_phase_order_mean: f64,
    pub frame_knorm_over_cap: f64,
    pub frame_over_fraction: f64,
    pub coeff_grad_over_k0: f64,
}

#[derive(Clone, Debug, Default)]
pub struct FieldAdjoints {
    pub rho: Option<Vec<f64>>,
    pub phase_fraction: Option<Vec<f64>>,
    pub rho_lat: Option<Vec<f64>>,
    pub pen_det: f64,
    pub pen_band: f64,
    pub pen_cond: f64,
}

#[derive(Clone, Debug, Default)]
pub struct ExtraAdjoints {
    pub mtilde: Option<Vec<f64>>,
    pub q: Option<Vec<f64>>,
    pub tau: Option<Vec<f64>>,
    pub grad_q: Option<Vec<f64>>,
    pub fold_weight: Option<Vec<f64>>,
    pub w_hat: Option<Vec<Vec<f64>>>,
    pub jac: Option<[[Vec<f64>; 3]; 3]>,
}

const A_IN: usize = 13;
const A_OUT: usize = 13;

fn stage_a<S: Scalar>(x: &[S; A_IN], lat: &LatticeConfig, ch: Channels) -> [S; A_OUT] {
    let mut out = [S::cst(0.0); A_OUT];
    if ch.stretch {
        for i in 0..3 {
            out[i] = (x[i].tanh() * lat.s_max).exp() - 1.0;
        }
    }
    if ch.family {
        let w: [S; N_BASIS] = std::array::from_fn(|i| x[3 + i]);
        let wh = normalise_coefficients(w, lat.w_eps);
        out[3..11].copy_from_slice(&wh);
    } else {
        out[3 + GYROID] = S::cst(1.0);
    }
    if ch.secondary {
        out[11] = x[11].tanh() * lat.sec_scale;
    }
    if ch.residual {
        out[12] = x[12].tanh() * lat.res_scale;
    }
    out
}

const C_PHI: usize = 0;
const C_JAC: usize = 3;
const C_WH: usize = 12;
const C_GW: usize = 20;
const C_AMP: usize = 44;
const C_GA: usize = 45;
const C_RT: usize = 48;
const C_GR: usize = 49;
const C_IN: usize = 52;

struct CellC<S> {
    f: S,
    gnorm: S,
    extra: [S; 3],
    phase_order: Option<S>,
    det: S,
    kn: [S; 3],
    r: S,
    pen_det: S,
    pen_band: S,
    pen_cond: S,
}

struct Consts {
    k0: f64,
    kmax: f64,
    r_max: f64,
}

fn stage_c<S: Scalar>(x: &[S; C_IN], lat: &LatticeConfig, ch: Channels, k: &Consts) -> CellC<S> {
    let phi = [x[C_PHI], x[C_PHI + 1], x[C_PHI + 2]];
    let jac: [[S; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|kk| x[C_JAC + 3 * i + kk]));
    let zero = S::cst(0.0);
    let mut extra = [zero; 3];
    let (mut f, mut dfdphi, w_hat);
    if ch.family {
        let (bv, bg) = basis_and_grad(phi);
        let wh: [S; N_BASIS] = std::array::from_fn(|i| x[C_WH + i]);
        f = wh[0] * bv[0];
        for i in 1..N_BASIS {
            f = f + wh[i] * bv[i];
        }
        dfdphi = [zero; 3];
        for (kk, d) in dfdphi.iter_mut().enumerate() {
            let mut s = wh[0] * bg[kk][0];
            for i in 1..N_BASIS {
                s = s + wh[i] * bg[kk][i];
            }
            *d = s;
        }
        for i in 0..N_BASIS {
            for (kk, e) in extra.iter_mut().enumerate() {
                *e = *e + bv[i] * x[C_GW + 3 * i + kk];
            }
        }
        w_hat = wh;
    } else {
        let [p1, p2, p3] = phi;
        let (c1, c2, c3) = (p1.cos(), p2.cos(), p3.cos());
        let (s1, s2, s3) = (p1.sin(), p2.sin(), p3.sin());
        f = s1 * c2 + s2 * c3 + s3 * c1;
        dfdphi = [c1 * c2 - s3 * s1, -s1 * s2 + c2 * c3, -s2 * s3 + c3 * c1];
        let mut wh = [zero; N_BASIS];
        wh[GYROID] = S::cst(1.0);
        w_hat = wh;
    }
    let mut sec_amp = None;
    if ch.secondary {
        let amp = x[C_AMP];
        sec_amp = Some(amp);
        let r = lat.sec_ratio;
        let (sv, sg) = basis_and_grad(phi.map(|p| p * r));
        let mut f2 = w_hat[0] * sv[0];
        for i in 1..N_BASIS {
            f2 = f2 + w_hat[i] * sv[i];
        }
        f = f + amp * f2;
        for kk in 0..3 {
            let mut s = w_hat[0] * sg[kk][0];
            for i in 1..N_BASIS {
                s = s + w_hat[i] * sg[kk][i];
            }
            dfdphi[kk] = dfdphi[kk] + amp * r * s;
        }
        for (kk, e) in extra.iter_mut().enumerate() {
            *e = *e + f2 * x[C_GA + kk];
        }
    }
    if ch.residual {
        f = f + x[C_RT];
        for (kk, e) in extra.iter_mut().enumerate() {
            *e = *e + x[C_GR + kk];
        }
    }
    let phase_order = (ch.family || ch.secondary).then(|| phase_harmonic_order(&w_hat, sec_amp, lat));
    let fg: [S; 3] = std::array::from_fn(|kk| {
        dfdphi[0] * jac[0][kk] + dfdphi[1] * jac[1][kk] + dfdphi[2] * jac[2][kk] + extra[kk]
    });
    let eps_g = lat.soft_eps_rel * k.k0;
    let gnorm = (fg[0] * fg[0] + fg[1] * fg[1] + fg[2] * fg[2] + eps_g * eps_g).sqrt();

    let j = &jac;
    let det = j[0][0] * (j[1][1] * j[2][2] - j[1][2] * j[2][1])
        - j[0][1] * (j[1][0] * j[2][2] - j[1][2] * j[2][0])
        + j[0][2] * (j[1][0] * j[2][1] - j[1][1] * j[2][0]);
    let k03 = k.k0.powi(3);
    let det_rel = det / k03;
    let pen_det = (-(det_rel - lat.det_floor)).relu().powi(2);
    let kn: [S; 3] = std::array::from_fn(|i| (j[i][0].powi(2) + j[i][1].powi(2) + j[i][2].powi(2)).sqrt());
    let mut pen_band = zero;
    for v in kn {
        pen_band = pen_band + (v / k.kmax - 1.0).relu().powi(2);
    }
    let mut frob2 = zero;
    for row in j {
        frob2 = frob2 + (row[0].powi(2) + row[1].powi(2) + row[2].powi(2));
    }
    let det_pos = det.max_c(1e-2 * k03);
    let r = (frob2 / 3.0) / det_pos.powf(2.0 / 3.0);
    let pen_cond = (r / k.r_max - 1.0).relu().powi(2);
    CellC { f, gnorm, extra, phase_order, det, kn, r, pen_det, pen_band, pen_cond }
}

const E_IN: usize = 7;

struct CellE<S> {
    rho: S,
    rho_lat: S,
    mtilde: S,
    cu: S,
    d: S,
    q: S,
    g: S,
    t: S,
    tau: S,
    nu: S,
    geff: S,
    grad_q: S,
}

struct EConsts {
    g_floor: f64,
    t_floor: f64,
    t_mid: f64,
    iface_len: f64,
}

fn stage_e<S: Scalar>(x: &[S; E_IN], lat: &LatticeConfig, o: &SynthOptions, e: &EConsts) -> CellE<S> {
    let [f, gnorm, blurred, ta, tnu, tm, tc] = *x;
    let geff = soft_floor(blurred, e.g_floor, FLOOR_WIDTH_REL * e.g_floor);
    let t_nom = ta.sigmoid() * (lat.t_max - lat.t_min) + lat.t_min;
    let t_eff = soft_floor(t_nom + o.t_offset, e.t_floor, FLOOR_WIDTH_REL * e.t_floor);
    let nu = if o.channels.mode { (tnu * o.beta_topo).sigmoid() } else { S::cst(0.0) };
    let folded = (f * f + lat.interface_eps.powi(2)).sqrt();
    let d = folded / geff;
    let one_m_nu = -nu + 1.0;
    let q = (one_m_nu * folded + nu * f) / geff;
    let tau = t_eff - nu * e.t_mid;
    let g = q - tau;
    let fsign = if f.val() >= 0.0 { 1.0 } else { -1.0 };
    let fold_slope = one_m_nu * fsign + nu;
    let sgn = if fold_slope.val() > 0.0 {
        1.0
    } else if fold_slope.val() < 0.0 {
        -1.0
    } else {
        0.0
    };
    let grad_q = fold_slope * sgn * (gnorm / geff);
    let rho_lat = (-g / e.iface_len).sigmoid();
    let mtilde = (tm * o.beta_mask).sigmoid();
    let rho = mtilde * rho_lat;
    let cu = if o.channels.material { (tc * o.beta_mat).sigmoid() } else { S::cst(0.5) };
    CellE { rho, rho_lat, mtilde, cu, d, q, g, t: t_eff, tau, nu, geff, grad_q }
}

pub struct SynthState {
    grids: Grids,
    lat: LatticeConfig,
    opts: SynthOptions,
    interp: Interp,
    ic: Vec<Vec<f64>>,
    c_in: Vec<[f64; C_IN]>,
    f: Vec<f64>,
    gnorm: Vec<f64>,
    blurred: Vec<f64>,
    consts: Consts,
    econsts: EConsts,
    pub fields: Fields,
}

fn used_components(ch: Channels) -> Vec<usize> {
    let mut v = vec![0, 1];
    if ch.phase {
        v.extend(2..5);
    }
    if ch.stretch {
        v.extend(5..8);
    }
    if ch.family {
        v.extend(8..16);
    }
    if ch.mode {
        v.push(16);
    }
    if ch.secondary {
        v.push(17);
    }
    if ch.residual {
        v.push(18);
    }
    if ch.material {
        v.push(19);
    }
    v
}

fn mean(v: &[f64]) -> f64 {
    crate::numpy::mean(v)
}


#[allow(clippy::too_many_lines)]
pub fn synthesize(
    controls: &Controls,
    grids: &Grids,
    lat: &LatticeConfig,
    opts: &SynthOptions,
) -> GResult<SynthState> {
    if controls.grid != grids.nc {
        return Err(GeometryError::Value(format!(
            "control grid {:?} does not match the geometry map's control nodes {:?}",
            controls.grid, grids.nc
        )));
    }
    let ch = opts.channels;
    let n = grids.n;
    let cells = grids.cells();
    let sp = grids.spacing;
    let k0 = grids.k0;
    let interp = Interp::new(grids.nc, &grids.ci);
    let used = used_components(ch);
    let mut ic: Vec<Vec<f64>> = vec![Vec::new(); 20];
    let comps: Vec<(usize, Vec<f64>)> =
        used.par_iter().map(|&c| (c, interp.forward(controls.component(c)))).collect();
    for (c, v) in comps {
        ic[c] = v;
    }
    let get = |c: usize, i: usize| ic[c].get(i).copied().unwrap_or(0.0);

    let a_out: Vec<[f64; A_OUT]> = (0..cells)
        .into_par_iter()
        .map(|i| {
            let x: [f64; A_IN] = std::array::from_fn(|k| match k {
                0..3 => get(5 + k, i),
                3..11 => get(8 + k - 3, i),
                11 => get(17, i),
                _ => get(18, i),
            });
            stage_a(&x, lat, ch)
        })
        .collect();
    let col = |k: usize| -> Vec<f64> { a_out.iter().map(|r| r[k]).collect() };

    let zero = vec![0.0; cells];
    let integ: [Vec<f64>; 3] = std::array::from_fn(|i| {
        if ch.stretch {
            cumintegrate(&col(i), n, sp[i], i).into_iter().map(|v| k0 * v).collect()
        } else {
            zero.clone()
        }
    });
    let dp: [Vec<f64>; 3] = std::array::from_fn(|i| if ch.phase { ic[2 + i].clone() } else { zero.clone() });
    let phi: [Vec<f64>; 3] =
        std::array::from_fn(|i| (0..cells).map(|c| grids.xc[i][c] * k0 + integ[i][c] + dp[i][c]).collect());
    let mut jac: [[Vec<f64>; 3]; 3] = Default::default();
    for i in 0..3 {
        for k in 0..3 {
            let gi = grad_center(&integ[i], n, sp, k);
            let gd = grad_center(&dp[i], n, sp, k);
            jac[i][k] = if i == k {
                gi.iter().zip(&gd).map(|(a, b)| k0 + a + b).collect()
            } else {
                gi.iter().zip(&gd).map(|(a, b)| a + b).collect()
            };
        }
    }
    let w_hat: Vec<Vec<f64>> = (0..N_BASIS).map(|i| col(3 + i)).collect();
    let gw: Vec<[Vec<f64>; 3]> = if ch.family {
        w_hat.iter().map(|w| std::array::from_fn(|k| grad_center(w, n, sp, k))).collect()
    } else {
        Vec::new()
    };
    let amp = col(11);
    let rterm = col(12);
    let ga: [Vec<f64>; 3] =
        std::array::from_fn(|k| if ch.secondary { grad_center(&amp, n, sp, k) } else { zero.clone() });
    let gr: [Vec<f64>; 3] =
        std::array::from_fn(|k| if ch.residual { grad_center(&rterm, n, sp, k) } else { zero.clone() });
    let c_in: Vec<[f64; C_IN]> = (0..cells)
        .map(|c| {
            let mut x = [0.0; C_IN];
            for i in 0..3 {
                x[C_PHI + i] = phi[i][c];
                for k in 0..3 {
                    x[C_JAC + 3 * i + k] = jac[i][k][c];
                }
            }
            for i in 0..N_BASIS {
                x[C_WH + i] = w_hat[i][c];
                if ch.family {
                    for k in 0..3 {
                        x[C_GW + 3 * i + k] = gw[i][k][c];
                    }
                }
            }
            x[C_AMP] = amp[c];
            x[C_RT] = rterm[c];
            for k in 0..3 {
                x[C_GA + k] = ga[k][c];
                x[C_GR + k] = gr[k][c];
            }
            x
        })
        .collect();
    drop(gw);

    let hmax = sp[0].max(sp[1]).max(sp[2]);
    let kmax = 2.0 * std::f64::consts::PI / (lat.min_elems_per_period * hmax);
    let cmax = lat.cond_max;
    let r_max = (cmax * cmax + 2.0) / (3.0 * cmax.powf(2.0 / 3.0));
    let consts = Consts { k0, kmax, r_max };
    let cres: Vec<CellC<f64>> = c_in.par_iter().map(|x| stage_c(x, lat, ch, &consts)).collect();
    let f: Vec<f64> = cres.iter().map(|c| c.f).collect();
    let gnorm: Vec<f64> = cres.iter().map(|c| c.gnorm).collect();
    let blurred = box_blur3(&gnorm, n, grids.blur_radius);

    let (iface_len, floored) =
        resolve_interface_length(opts.interface_w, grids.h, opts.interface_len, INTERFACE_FLOOR_ELEMS);
    let econsts = EConsts {
        g_floor: lat.grad_floor_rel * k0,
        t_floor: 0.05 * lat.t_min,
        t_mid: 0.5 * (lat.t_min + lat.t_max),
        iface_len,
    };
    let eres: Vec<CellE<f64>> = (0..cells)
        .into_par_iter()
        .map(|c| {
            let x = [f[c], gnorm[c], blurred[c], get(0, c), get(16, c), get(1, c), get(19, c)];
            stage_e(&x, lat, opts, &econsts)
        })
        .collect();

    #[allow(clippy::cast_precision_loss)]
    let ncell = cells as f64;
    let det_rel: Vec<f64> = cres.iter().map(|c| c.det / k0.powi(3)).collect();
    let pen_det = mean(&cres.iter().map(|c| c.pen_det).collect::<Vec<_>>());
    let mut pen_band = 0.0;
    for r in 0..3 {
        pen_band += mean(&cres.iter().map(|c| (c.kn[r] / kmax - 1.0).max(0.0).powi(2)).collect::<Vec<_>>());
    }
    let kn_cell: Vec<f64> = cres.iter().map(|c| c.kn.iter().fold(0.0f64, |m, v| m.max(*v))).collect();
    let frame_knorm = kn_cell.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    #[allow(clippy::cast_precision_loss)]
    let frame_over = kn_cell.iter().filter(|v| **v > kmax).count() as f64 / ncell;
    let kn_lat: Vec<f64> =
        cres.iter().zip(&kn_cell).map(|(c, k)| c.phase_order.map_or(*k, |p| k * p)).collect();
    let knmax = kn_lat.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    #[allow(clippy::cast_precision_loss)]
    let band_over = kn_lat.iter().filter(|v| **v > kmax).count() as f64 / ncell;
    let orders: Vec<f64> = cres.iter().filter_map(|c| c.phase_order).collect();
    let (po_max, po_mean) = if orders.is_empty() {
        (1.0, 1.0)
    } else {
        (orders.iter().copied().fold(f64::NEG_INFINITY, f64::max), mean(&orders))
    };
    let coeff = cres
        .iter()
        .map(|c| (c.extra[0].powi(2) + c.extra[1].powi(2) + c.extra[2].powi(2)).sqrt())
        .fold(f64::NEG_INFINITY, f64::max)
        / k0;
    let fields = Fields {
        rho: eres.iter().map(|e| e.rho).collect(),
        rho_lat: eres.iter().map(|e| e.rho_lat).collect(),
        mtilde: eres.iter().map(|e| e.mtilde).collect(),
        phase_fraction: eres.iter().map(|e| e.cu).collect(),
        interface_len: iface_len,
        interface_elems: iface_len / grids.h,
        interface_floored: floored,
        f: f.clone(),
        d: eres.iter().map(|e| e.d).collect(),
        q: eres.iter().map(|e| e.q).collect(),
        g: eres.iter().map(|e| e.g).collect(),
        t: eres.iter().map(|e| e.t).collect(),
        tau: eres.iter().map(|e| e.tau).collect(),
        nu: eres.iter().map(|e| e.nu).collect(),
        gnorm: gnorm.clone(),
        geff: eres.iter().map(|e| e.geff).collect(),
        grad_d: eres.iter().zip(&gnorm).map(|(e, g)| g / e.geff).collect(),
        grad_q: eres.iter().map(|e| e.grad_q).collect(),
        phi,
        w_hat,
        fold_weight: eres.iter().map(|e| 1.0 - e.nu).collect(),
        jac,
        pen_det,
        pen_band,
        pen_cond: mean(&cres.iter().map(|c| c.pen_cond).collect::<Vec<_>>()),
        det_rel_min: det_rel.iter().copied().fold(f64::INFINITY, f64::min),
        cond_max: cres.iter().map(|c| c.r).fold(f64::NEG_INFINITY, f64::max),
        knorm_over_cap: knmax / kmax,
        band_over_fraction: band_over,
        band_min_elems_per_period: 2.0 * std::f64::consts::PI / (knmax * grids.h),
        band_phase_order_max: po_max,
        band_phase_order_mean: po_mean,
        frame_knorm_over_cap: frame_knorm / kmax,
        frame_over_fraction: frame_over,
        coeff_grad_over_k0: coeff,
    };
    Ok(SynthState {
        grids: grids.clone(),
        lat: lat.clone(),
        opts: opts.clone(),
        interp,
        ic,
        c_in,
        f,
        gnorm,
        blurred,
        consts,
        econsts,
        fields,
    })
}

const VJP_CHUNK: usize = 2048;

fn pointwise_vjp<const NI: usize>(
    cells: usize,
    input: &(dyn Fn(usize) -> [f64; NI] + Sync),
    weighted: &(dyn Fn(usize, &[Rv; NI]) -> Option<Rv> + Sync),
) -> GResult<Vec<[f64; NI]>> {
    let chunks: Vec<GResult<Vec<[f64; NI]>>> = (0..cells)
        .into_par_iter()
        .chunks(VJP_CHUNK)
        .map(|idx| {
            let mut tape = RevTape::begin()?;
            let mut out = Vec::with_capacity(idx.len());
            for c in idx {
                let base = tape.len();
                let x = input(c);
                let leaves: [Rv; NI] = std::array::from_fn(|k| tape.leaf(x[k]));
                let mark = tape.len();
                match weighted(c, &leaves) {
                    Some(y) => {
                        tape.sweep_to(y, 1.0, mark);
                        tape.finish();
                        out.push(std::array::from_fn(|k| tape.adjoint(leaves[k])));
                    }
                    None => out.push([0.0; NI]),
                }
                tape.sweep_to(Rv::cst(0.0), 0.0, base);
            }
            Ok(out)
        })
        .collect();
    let mut all = Vec::with_capacity(cells);
    for c in chunks {
        all.extend(c?);
    }
    Ok(all)
}

impl SynthState {
    #[must_use]
    pub fn grids(&self) -> &Grids {
        &self.grids
    }


    #[allow(clippy::too_many_lines)]
    pub fn vjp(&self, adj: &FieldAdjoints) -> GResult<Vec<f64>> {
        self.vjp_with(adj, &ExtraAdjoints::default())
    }


    #[allow(clippy::too_many_lines)]
    pub fn vjp_with(&self, adj: &FieldAdjoints, extra: &ExtraAdjoints) -> GResult<Vec<f64>> {
        let g = &self.grids;
        let cells = g.cells();
        let n = g.n;
        let sp = g.spacing;
        let ch = self.opts.channels;
        let lat = &self.lat;
        let wrows = extra.w_hat.iter().flatten();
        let jrows = extra.jac.iter().flat_map(|j| j.iter().flatten());
        for a in [
            &adj.rho,
            &adj.phase_fraction,
            &adj.rho_lat,
            &extra.mtilde,
            &extra.q,
            &extra.tau,
            &extra.grad_q,
            &extra.fold_weight,
        ]
        .into_iter()
        .flatten()
        .chain(wrows)
        .chain(jrows)
        {
            if a.len() != cells {
                return Err(GeometryError::Value("field adjoint does not match the geometry grid".into()));
            }
        }
        let get = |c: usize, i: usize| self.ic[c].get(i).copied().unwrap_or(0.0);
        let zero = vec![0.0; cells];
        let arho = adj.rho.as_deref().unwrap_or(&zero);
        let aphase = adj.phase_fraction.as_deref().unwrap_or(&zero);
        let alat = adj.rho_lat.as_deref().unwrap_or(&zero);
        if extra.w_hat.as_ref().is_some_and(|w| w.len() != N_BASIS) {
            return Err(GeometryError::Value("w_hat adjoint needs one row per basis function".into()));
        }
        let am = extra.mtilde.as_deref().unwrap_or(&zero);
        let aq = extra.q.as_deref().unwrap_or(&zero);
        let at = extra.tau.as_deref().unwrap_or(&zero);
        let agq = extra.grad_q.as_deref().unwrap_or(&zero);
        let afw = extra.fold_weight.as_deref().unwrap_or(&zero);

        let opts = &self.opts;
        let ec = &self.econsts;
        let e_adj = pointwise_vjp::<E_IN>(
            cells,
            &|c| [self.f[c], self.gnorm[c], self.blurred[c], get(0, c), get(16, c), get(1, c), get(19, c)],
            &|c, x| {
                let (wr, wp, wl) = (arho[c], aphase[c], alat[c]);
                let (wm, wq, wt, wg, wf) = (am[c], aq[c], at[c], agq[c], afw[c]);
                if [wr, wp, wl, wm, wq, wt, wg, wf].iter().all(|v| *v == 0.0) {
                    return None;
                }
                let e = stage_e(x, lat, opts, ec);
                Some(
                    e.rho * wr
                        + e.cu * wp
                        + e.rho_lat * wl
                        + e.mtilde * wm
                        + e.q * wq
                        + e.tau * wt
                        + e.grad_q * wg
                        + (-e.nu + 1.0) * wf,
                )
            },
        )?;
        let mut comp_adj: Vec<Vec<f64>> = vec![Vec::new(); 20];
        let col = |v: &[[f64; E_IN]], k: usize| -> Vec<f64> { v.iter().map(|r| r[k]).collect() };
        comp_adj[0] = col(&e_adj, 3);
        if ch.mode {
            comp_adj[16] = col(&e_adj, 4);
        }
        comp_adj[1] = col(&e_adj, 5);
        if ch.material {
            comp_adj[19] = col(&e_adj, 6);
        }
        let a_f = col(&e_adj, 0);
        let mut a_gnorm = col(&e_adj, 1);
        let bt = box_blur3_t(&col(&e_adj, 2), n, g.blur_radius);
        for (a, b) in a_gnorm.iter_mut().zip(&bt) {
            *a += b;
        }

        #[allow(clippy::cast_precision_loss)]
        let ncell = cells as f64;
        let (wd, wb, wc) = (adj.pen_det / ncell, adj.pen_band / ncell, adj.pen_cond / ncell);
        let consts = &self.consts;
        let c_adj = pointwise_vjp::<C_IN>(cells, &|c| self.c_in[c], &|c, x| {
            let (af, ag) = (a_f[c], a_gnorm[c]);
            if af == 0.0 && ag == 0.0 && wd == 0.0 && wb == 0.0 && wc == 0.0 {
                return None;
            }
            let r = stage_c(x, lat, ch, consts);
            Some(r.f * af + r.gnorm * ag + r.pen_det * wd + r.pen_band * wb + r.pen_cond * wc)
        })?;
        let ccol = |k: usize| -> Vec<f64> { c_adj.iter().map(|r| r[k]).collect() };

        let k0 = g.k0;
        let mut a_integ: [Vec<f64>; 3] = std::array::from_fn(|i| ccol(C_PHI + i));
        let mut a_dp: [Vec<f64>; 3] = std::array::from_fn(|i| ccol(C_PHI + i));
        for i in 0..3 {
            for k in 0..3 {
                let mut aj = ccol(C_JAC + 3 * i + k);
                if let Some(j) = &extra.jac {
                    for (a, b) in aj.iter_mut().zip(&j[i][k]) {
                        *a += b;
                    }
                }
                grad_center_t(&aj, n, sp, k, &mut a_integ[i]);
                grad_center_t(&aj, n, sp, k, &mut a_dp[i]);
            }
        }
        let mut a_aout: Vec<Vec<f64>> = vec![vec![0.0; cells]; A_OUT];
        if ch.stretch {
            for i in 0..3 {
                let scaled: Vec<f64> = a_integ[i].iter().map(|v| v * k0).collect();
                a_aout[i] = cumintegrate_t(&scaled, n, sp[i], i);
            }
        }
        if ch.phase {
            for i in 0..3 {
                comp_adj[2 + i] = std::mem::take(&mut a_dp[i]);
            }
        }
        for i in 0..N_BASIS {
            let mut a = ccol(C_WH + i);
            if let Some(w) = &extra.w_hat {
                for (x, y) in a.iter_mut().zip(&w[i]) {
                    *x += y;
                }
            }
            if ch.family {
                for k in 0..3 {
                    grad_center_t(&ccol(C_GW + 3 * i + k), n, sp, k, &mut a);
                }
            }
            a_aout[3 + i] = a;
        }
        let mut a_amp = ccol(C_AMP);
        let mut a_rt = ccol(C_RT);
        for k in 0..3 {
            if ch.secondary {
                grad_center_t(&ccol(C_GA + k), n, sp, k, &mut a_amp);
            }
            if ch.residual {
                grad_center_t(&ccol(C_GR + k), n, sp, k, &mut a_rt);
            }
        }
        a_aout[11] = a_amp;
        a_aout[12] = a_rt;

        let a_in = pointwise_vjp::<A_IN>(
            cells,
            &|i| {
                std::array::from_fn(|k| match k {
                    0..3 => get(5 + k, i),
                    3..11 => get(8 + k - 3, i),
                    11 => get(17, i),
                    _ => get(18, i),
                })
            },
            &|c, x| {
                let o = stage_a(x, lat, ch);
                let mut acc: Option<Rv> = None;
                for (k, v) in o.iter().enumerate() {
                    let w = a_aout[k][c];
                    if w != 0.0 {
                        let t = *v * w;
                        acc = Some(match acc {
                            Some(a) => a + t,
                            None => t,
                        });
                    }
                }
                acc
            },
        )?;
        let acol = |k: usize| -> Vec<f64> { a_in.iter().map(|r| r[k]).collect() };
        if ch.stretch {
            for i in 0..3 {
                comp_adj[5 + i] = acol(i);
            }
        }
        if ch.family {
            for i in 0..N_BASIS {
                comp_adj[8 + i] = acol(3 + i);
            }
        }
        if ch.secondary {
            comp_adj[17] = acol(11);
        }
        if ch.residual {
            comp_adj[18] = acol(12);
        }
        let nctl = g.n_control();
        let parts: Vec<(usize, Vec<f64>)> = comp_adj
            .par_iter()
            .enumerate()
            .filter(|(_, v)| !v.is_empty())
            .map(|(c, v)| (c, self.interp.transpose(v)))
            .collect();
        let mut out = vec![0.0; 20 * nctl];
        for (c, v) in parts {
            out[c * nctl..(c + 1) * nctl].copy_from_slice(&v);
        }
        Ok(out)
    }
}

#[must_use]
pub fn volume(fields: &Fields) -> f64 {
    mean(&fields.rho)
}
