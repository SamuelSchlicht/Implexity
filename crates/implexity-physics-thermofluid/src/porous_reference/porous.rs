// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;

use super::ad::{A, interp};

pub const GYROID_AV_L: f64 = 3.1284;
pub const GYROID_SHEET_AV_L: f64 = 2.0 * GYROID_AV_L;
pub const GYROID_K_OVER_L2_JT: f64 = 2.2889e-3;
pub const C_F_MIN: f64 = 0.15;
pub const C_F_MAX: f64 = 2.0;
pub const C_F_CLAMP_SOFT: f64 = 0.02;
pub const A_V_WIDTH_MIN: f64 = 0.35;
pub const CLOSURE_FAMILIES: [&str; 7] =
    ["schwarz_p", "gyroid", "diamond", "iwp", "neovius", "frd", "lidinoid"];
pub const KOZENY_POLY: [(&str, [f64; 3]); 5] = [
    ("schwarz_p", [1.3650, -1.5977, 0.0678]),
    ("gyroid", [5.0103, -13.5560, 9.8379]),
    ("diamond", [4.7890, -13.3199, 10.0122]),
    ("iwp", [3.6756, -9.1545, 6.2688]),
    ("lidinoid", [5.2544, -14.3459, 10.5648]),
];
pub const KOZENY_POLY_SPLIT_P: [f64; 3] = [4.3139, -11.4400, 8.2652];
pub const KOZENY_TABLE_FRD: ([f64; 7], [f64; 7]) =
    ([0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70], [0.1073, 0.2990, 0.3369, 0.3071, 0.3452, 0.2710, 0.2890]);
pub const KOZENY_TABLE_NEOVIUS: ([f64; 8], [f64; 8]) = (
    [0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70],
    [0.0281, 0.1036, 0.2064, 0.1973, 0.2679, 0.3063, 0.2565, 0.2995],
);
pub const KOZENY_RMS: [(&str, f64); 7] = [
    ("schwarz_p", 0.0245),
    ("gyroid", 0.0165),
    ("diamond", 0.0810),
    ("iwp", 0.0257),
    ("lidinoid", 0.0914),
    ("frd", 0.0375),
    ("neovius", 0.0218),
];
pub const C_K_MIN: f64 = 0.01;
pub const C_K_MIN_SOFT: f64 = 0.002;
pub const PERCOLATION_PHI: [(&str, f64); 2] = [("frd", 0.375), ("neovius", 0.325)];
pub const PERCOLATION_WIDTH: f64 = 0.005;
pub const FORCHHEIMER_FAMILIES: [&str; 3] = ["gyroid", "diamond", "split_p"];
pub const FORCHHEIMER_POW: [(f64, f64); 3] = [(0.0908, -1.81), (0.0666, -1.69), (0.1270, -1.83)];
pub const FORCHHEIMER_ASSIGN: [(&str, &str); 7] = [
    ("schwarz_p", "split_p"),
    ("gyroid", "gyroid"),
    ("diamond", "diamond"),
    ("iwp", "gyroid"),
    ("neovius", "split_p"),
    ("frd", "diamond"),
    ("lidinoid", "gyroid"),
];
const NU_B_C: f64 = 0.0964;
const NU_B_RE: f64 = 0.7136;
const NU_PR: f64 = 0.40;
const NU_R_C: f64 = 0.49;
const NU_R_RE: f64 = 0.62;
const NU_DB_C: f64 = 0.023;
const NU_DB_RE: f64 = 0.8;
pub const NU_BLEND_RE: f64 = 3000.0;
pub const NU_BLEND_WIDTH: f64 = 0.35;
pub const NU_BLEND_EXP_FLOOR: f64 = 0.31;
pub const NU_BLEND_WIDTH_RESOLVED: f64 = 0.70;
const NU_KTA_C1: f64 = 1.27;
const NU_KTA_E1: f64 = 0.36;
const NU_KTA_C2: f64 = 0.033;
const NU_KTA_E2: f64 = 0.60;
pub const NU_KTA_EPS_FLOOR: f64 = 1e-3;
const NU_WAKAO_C: f64 = 1.1;
const NU_WAKAO_E: f64 = 0.6;
pub const NU_FLOOR: f64 = 4.36;
pub const NU_FLOOR_SOFT: f64 = 0.5;
const TINY: f64 = 1e-300;
pub const PHI_EPS: f64 = 1e-4;
pub const RE_EPS: f64 = 1e-12;
pub const NUSSELT_LAWS: [&str; 7] =
    ["blend", "blend_resolved", "brambati", "reynolds", "dittus_boelter", "kta", "wakao"];

pub fn soft_band<S: Scalar>(x: S, lo: f64, hi: f64, s: f64) -> S {
    let y = ((x - lo) / s).softplus() * s + lo;
    -(((-y + hi) / s).softplus() * s) + hi
}

pub fn soft_floor<S: Scalar>(x: S, lo: f64, s: f64) -> S {
    ((x - lo) / s).softplus() * s + lo
}

#[must_use]
pub fn coarea_length(h: f64, interface_w: f64, interface_len: Option<f64>, w_min: f64) -> f64 {
    let w_in = interface_len.map_or(interface_w, |l| l / h);
    w_in.max(w_min) * h
}

pub fn coarea_density<S: Scalar>(q: S, tau: S, grad: S, fold_weight: S, length: f64, mirror: bool) -> S {
    let ra = ((tau - q) / length).sigmoid();
    let mut dens = ra * (-ra + 1.0);
    if mirror {
        let rb = ((tau + q) / length).sigmoid();
        dens += fold_weight * rb * (-rb + 1.0);
    }
    grad * dens / length
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn specific_surface<'g>(
    q: A<'g>,
    tau: A<'g>,
    h: f64,
    interface_w: f64,
    smooth_len: Option<f64>,
    grad_d: Option<A<'g>>,
    mirror: bool,
    fold_weight: A<'g>,
    interface_len: Option<f64>,
) -> A<'g> {
    let g = q.graph();
    let length = coarea_length(h, interface_w, interface_len, A_V_WIDTH_MIN);
    let grad = grad_d.unwrap_or_else(|| g.scalar(1.0));
    let a_v = g.mapn([q, tau, grad, fold_weight], move |[q, t, gd, fw]| {
        coarea_density(q, t, gd, fw, length, mirror)
    });
    match smooth_len {
        Some(sl) if sl > 0.0 => {
            let radius = 1usize.max(py_round(sl / h));
            super::ops::box_blur3(a_v, radius)
        }
        _ => a_v,
    }
}

#[must_use]
pub fn py_round(x: f64) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let r = x.round_ties_even() as usize;
    r
}

pub fn percolation_gate<S: Scalar>(phi: S, family: &str) -> S {
    match PERCOLATION_PHI.iter().find(|(f, _)| *f == family) {
        Some((_, c)) => ((phi - *c) / PERCOLATION_WIDTH).sigmoid(),
        None => S::one(),
    }
}

pub fn family_kozeny<S: Scalar>(phi: S, family: &str) -> S {
    let ck = if let Some((_, c)) = KOZENY_POLY.iter().find(|(f, _)| *f == family) {
        phi * c[1] + c[0] + phi * c[2] * phi
    } else if family == "frd" {
        interp(phi, &KOZENY_TABLE_FRD.0, &KOZENY_TABLE_FRD.1)
    } else {
        interp(phi, &KOZENY_TABLE_NEOVIUS.0, &KOZENY_TABLE_NEOVIUS.1)
    };
    let ck = soft_floor(ck, C_K_MIN, C_K_MIN_SOFT);
    if PERCOLATION_PHI.iter().any(|(f, _)| *f == family) { ck * percolation_gate(phi, family) } else { ck }
}

pub fn kozeny_gyroid<S: Scalar>(phi: S) -> S {
    let [a, b, c] = KOZENY_POLY[1].1;
    phi * b + a + phi * c * phi
}

pub fn kozeny_mixed<S: Scalar>(phi: S, w: &[S; 7]) -> S {
    let mut s = S::zero();
    for (j, f) in CLOSURE_FAMILIES.iter().enumerate() {
        s += w[j] * family_kozeny(phi, f);
    }
    s
}

pub fn forchheimer_gyroid<S: Scalar>(phi: S, clamp: bool) -> S {
    let phi_s = (phi * phi + PHI_EPS * PHI_EPS).sqrt();
    let cf = phi_s.powf(-1.81) * 0.0908;
    if clamp { soft_band(cf, C_F_MIN, C_F_MAX, C_F_CLAMP_SOFT) } else { cf }
}

pub fn forchheimer_mixed<S: Scalar>(phi: S, w: &[S; 7], clamp: bool) -> S {
    let phi_s = (phi * phi + PHI_EPS * PHI_EPS).sqrt();
    let mut cf = S::zero();
    for (i, pubf) in FORCHHEIMER_FAMILIES.iter().enumerate() {
        let mut wi = S::zero();
        for (j, f) in CLOSURE_FAMILIES.iter().enumerate() {
            if FORCHHEIMER_ASSIGN.iter().any(|(a, b)| a == f && b == pubf) {
                wi += w[j];
            }
        }
        let (a, b) = FORCHHEIMER_POW[i];
        cf += wi * (phi_s.powf(b) * a);
    }
    if clamp { soft_band(cf, C_F_MIN, C_F_MAX, C_F_CLAMP_SOFT) } else { cf }
}

pub fn forchheimer_number<S: Scalar>(k: S, cf: S, u_s: S, rho_f: f64, mu: f64) -> S {
    cf * rho_f * k.sqrt() * u_s / mu
}

pub fn effective_mobility<S: Scalar>(g: S, k: S, cf: S, rho_f: f64, mu: f64) -> S {
    let a = S::from_f64(mu) / (k + TINY);
    let b = cf * rho_f / (k + TINY).sqrt();
    S::from_f64(2.0) / (a + (a * a + b * g * 4.0).sqrt() + TINY)
}

pub fn darcy_forchheimer_velocity<S: Scalar>(g: S, k: S, cf: S, rho_f: f64, mu: f64) -> S {
    let a = S::from_f64(mu) / (k + TINY);
    let b = cf * rho_f / (k + TINY).sqrt();
    g * 2.0 / (a + (a * a + b * g * 4.0).sqrt() + TINY)
}

pub fn nusselt_brambati<S: Scalar>(re: S, pr: f64) -> S {
    (re + RE_EPS).powf(NU_B_RE) * NU_B_C * pr.powf(NU_PR)
}

pub fn nusselt_reynolds<S: Scalar>(re: S, pr: f64) -> S {
    (re + RE_EPS).powf(NU_R_RE) * NU_R_C * pr.powf(NU_PR)
}

pub fn nusselt_dittus_boelter<S: Scalar>(re: S, pr: f64) -> S {
    (re + RE_EPS).powf(NU_DB_RE) * NU_DB_C * pr.powf(NU_PR)
}

fn particle_eps<S: Scalar>(eps: S) -> (S, S) {
    let e = (eps * eps + NU_KTA_EPS_FLOOR * NU_KTA_EPS_FLOOR).sqrt();
    let one_m = -e + 1.0;
    let e = -(one_m * one_m + NU_KTA_EPS_FLOOR * NU_KTA_EPS_FLOOR).sqrt() + 1.0;
    let solid = (-e + 1.0) * 1.5;
    (e, solid)
}

pub fn nusselt_kta<S: Scalar>(re: S, pr: f64, eps: S) -> S {
    let (e, solid) = particle_eps(eps);
    let re_p = solid * (re + RE_EPS);
    let nu_p = e.powf(-1.18) * (NU_KTA_C1 * pr.powf(1.0 / 3.0)) * re_p.powf(NU_KTA_E1)
        + e.powf(-1.07) * (NU_KTA_C2 * pr.powf(0.5)) * re_p.powf(NU_KTA_E2);
    nu_p * e / solid
}

pub fn nusselt_wakao<S: Scalar>(re: S, pr: f64, eps: S) -> S {
    let (e, solid) = particle_eps(eps);
    let re_p = solid * (re + RE_EPS);
    let nu_p = re_p.powf(NU_WAKAO_E) * (NU_WAKAO_C * pr.powf(1.0 / 3.0)) + 2.0;
    nu_p * e / solid
}

pub fn nusselt<S: Scalar>(
    re: S,
    pr: f64,
    laminar_floor: bool,
    law: Option<&str>,
    eps: S,
) -> Result<S, String> {
    let width = match law {
        None | Some("blend") => Some(NU_BLEND_WIDTH),
        Some("blend_resolved") => Some(NU_BLEND_WIDTH_RESOLVED),
        Some("brambati" | "reynolds" | "dittus_boelter" | "kta" | "wakao") => None,
        Some(other) => {
            return Err(format!("unknown nusselt law '{other}'; known: {}", NUSSELT_LAWS.join(", ")));
        }
    };
    let nu = match (width, law) {
        (Some(wd), _) => {
            let log_re = (re + RE_EPS).ln();
            let w = ((log_re - NU_BLEND_RE.ln()) / wd).sigmoid();
            (-w + 1.0) * nusselt_reynolds(re, pr) + w * nusselt_brambati(re, pr)
        }
        (None, Some("brambati")) => nusselt_brambati(re, pr),
        (None, Some("reynolds")) => nusselt_reynolds(re, pr),
        (None, Some("dittus_boelter")) => nusselt_dittus_boelter(re, pr),
        (None, Some("kta")) => nusselt_kta(re, pr, eps),
        _ => nusselt_wakao(re, pr, eps),
    };
    Ok(if laminar_floor { soft_floor(nu, NU_FLOOR, NU_FLOOR_SOFT) } else { nu })
}

#[must_use]
pub fn gajetti_power_law_permeability(phi: f64, l_cell: f64) -> f64 {
    0.0156 * phi.powf(2.78) * l_cell * l_cell
}

pub fn permeability<S: Scalar>(a_v: S, phi: S) -> S {
    kozeny_gyroid(phi) * phi.powi(5) / (a_v * a_v + TINY)
}

pub fn hydraulic_diameter<S: Scalar>(a_v: S, phi: S) -> S {
    phi * 4.0 / (a_v + TINY)
}
