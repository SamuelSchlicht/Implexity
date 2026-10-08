// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::OnceLock;
use implexity_ad::{Dual, HyperDual, Jet3, Scalar};
use implexity_physics_thermofluid::real_fluid_screening::RU;
use serde_json::{Value, json};
use crate::errors::{PResult, ModelError};
use crate::pyval::{fmt_e, repr_float};

pub const SPECIES_ORDER: [&str; 9] = ["CH4", "O2", "H2O", "CO2", "CO", "H2", "OH", "H", "O"];

pub const REACTANT_SPECIES: [&str; 2] = ["CH4", "O2"];

pub const SWITCH_CERTIFICATE: f64 = 1e-8;

const ATOMS: [[f64; 3]; 9] = [
    [1.0, 4.0, 0.0],
    [0.0, 0.0, 2.0],
    [0.0, 2.0, 1.0],
    [1.0, 0.0, 2.0],
    [1.0, 0.0, 1.0],
    [0.0, 2.0, 0.0],
    [0.0, 1.0, 1.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
];

pub const MW: [f64; 9] =
    [0.016_043, 0.031_999, 0.018_015, 0.044_010, 0.028_010, 0.002_016, 0.017_007, 0.001_008, 0.015_999];

const HF298: [f64; 9] =
    [-74870.0, 0.0, -241_826.0, -393_522.0, -110_527.0, 0.0, 37280.0, 217_999.0, 249_173.0];

const S298: [f64; 9] = [186.25, 205.152, 188.834, 213.795, 197.660, 130.680, 183.708, 114.717, 161.058];

const T_ANCHORS: [f64; 3] = [298.15, 1500.0, 3000.0];

const P_REF: f64 = 1.0e5;

const T_REF: f64 = 298.15;

const TMIN_CLAMP: f64 = 250.0;

const TMAX_CLAMP: f64 = 6000.0;

const RESIDUAL_SCALE: [f64; 5] = [1.0, 1.0, 1.0, 1.0, 1.0e6];

fn cp_anchors() -> [[f64; 3]; 9] {
    let mono = 5.0 * RU / 2.0;
    [
        [35.31, 74.85, 87.00],
        [29.38, 36.55, 41.90],
        [33.60, 47.17, 55.83],
        [37.13, 58.38, 62.35],
        [29.14, 34.99, 38.94],
        [28.83, 32.88, 41.55],
        [29.89, 34.99, 38.65],
        [mono; 3],
        [mono; 3],
    ]
}

fn solve<S: Scalar, const N: usize>(m: [[f64; N]; N], rhs: [S; N]) -> [S; N] {
    let mut a = m;
    let mut b = rhs;
    for k in 0..N {
        let mut piv = k;
        for r in k + 1..N {
            if a[r][k].abs() > a[piv][k].abs() {
                piv = r;
            }
        }
        a.swap(k, piv);
        b.swap(k, piv);
        for r in k + 1..N {
            let f = a[r][k] / a[k][k];
            let pivot = a[k];
            for (v, pk) in a[r].iter_mut().zip(pivot).skip(k) {
                *v -= f * pk;
            }
            let bk = b[k];
            b[r] -= bk * f;
        }
    }
    let mut x = [S::zero(); N];
    for k in (0..N).rev() {
        let mut s = b[k];
        for c in k + 1..N {
            s -= x[c] * a[k][c];
        }
        x[k] = s / a[k][k];
    }
    x
}

struct Nasa {
    a15: [[f64; 5]; 9],
    a67: [[f64; 2]; 9],
}

fn nasa() -> &'static Nasa {
    static CELL: OnceLock<Nasa> = OnceLock::new();
    CELL.get_or_init(|| {
        let t = T_ANCHORS;
        let a: [[f64; 3]; 3] = std::array::from_fn(|r| [1.0, t[r], t[r] * t[r]]);
        let anchors = cp_anchors();
        let mut a15 = [[0.0; 5]; 9];
        let mut a67 = [[0.0; 2]; 9];
        for i in 0..9 {
            let c: [f64; 3] = solve(a, anchors[i].map(|v| v / RU));
            a15[i] = [c[0], c[1], c[2], 0.0, 0.0];
            let tr = T_REF;
            a67[i][0] = HF298[i] / RU - (c[0] * tr + c[1] * tr * tr / 2.0 + c[2] * tr.powf(3.0) / 3.0);
            a67[i][1] = S298[i] / RU - (c[0] * tr.ln() + c[1] * tr + c[2] * tr * tr / 2.0);
        }
        Nasa { a15, a67 }
    })
}

pub fn cp_over_r<S: Scalar>(t: S) -> [S; 9] {
    let n = nasa();
    std::array::from_fn(|i| {
        let a = n.a15[i];
        S::from_f64(a[0]) + t * a[1] + t * a[2] * t + t.powi(3) * a[3] + t.powi(4) * a[4]
    })
}

pub fn h_over_rt<S: Scalar>(t: S) -> [S; 9] {
    let n = nasa();
    std::array::from_fn(|i| {
        let a = n.a15[i];
        S::from_f64(a[0])
            + t * a[1] / 2.0
            + t * a[2] * t / 3.0
            + t.powi(3) * a[3] / 4.0
            + t.powi(4) * a[4] / 5.0
            + S::from_f64(n.a67[i][0]) / t
    })
}

pub fn s_over_r<S: Scalar>(t: S) -> [S; 9] {
    let n = nasa();
    std::array::from_fn(|i| {
        let a = n.a15[i];
        t.ln() * a[0]
            + t * a[1]
            + t * a[2] * t / 2.0
            + t.powi(3) * a[3] / 3.0
            + t.powi(4) * a[4] / 4.0
            + n.a67[i][1]
    })
}

pub fn mu_over_rt<S: Scalar>(t: S) -> [S; 9] {
    let (h, s) = (h_over_rt(t), s_over_r(t));
    std::array::from_fn(|i| h[i] - s[i])
}

pub fn h_species_j_per_mol<S: Scalar>(t: S) -> [S; 9] {
    h_over_rt(t).map(|h| h * RU * t)
}

pub fn reactant_enthalpy_j_per_kg<S: Scalar>(y: [S; 2], t: [S; 2]) -> S {
    let h0 = h_species_j_per_mol(t[0])[0];
    let h1 = h_species_j_per_mol(t[1])[1];
    y[0] * h0 / MW[0] + y[1] * h1 / MW[1]
}

fn log_moles<S: Scalar>(u: &[S; 5], p: S) -> [S; 9] {
    let mu = mu_over_rt(u[4]);
    let lp = (p / P_REF).ln();
    std::array::from_fn(|i| u[0] * ATOMS[i][0] + u[1] * ATOMS[i][1] + u[2] * ATOMS[i][2] - mu[i] - lp + u[3])
}

fn sum<S: Scalar>(v: impl IntoIterator<Item = S>) -> S {
    v.into_iter().fold(S::zero(), |a, b| a + b)
}

pub fn residuals<S: Scalar>(u: &[S; 5], p: S, b: &[S; 3], h_mixed: S) -> [S; 5] {
    let lnn = log_moles(u, p);
    let n: [S; 9] = lnn.map(|x| x.clip(-80.0, 80.0).exp());
    let r_el = |j: usize| sum((0..9).map(|i| n[i] * ATOMS[i][j])) - b[j];
    let rn = sum(n) - u[3].exp();
    let hs = h_species_j_per_mol(u[4]);
    let rh = sum((0..9).map(|i| n[i] * hs[i])) - h_mixed;
    let raw = [r_el(0), r_el(1), r_el(2), rn, rh];
    std::array::from_fn(|k| raw[k] / RESIDUAL_SCALE[k])
}

fn norm(r: &[f64; 5]) -> f64 {
    r.iter().map(|x| x * x).sum::<f64>().sqrt()
}

fn jacobian(u: &[f64; 5], p: f64, b: &[f64; 3], h: f64) -> [[f64; 5]; 5] {
    let ud: [Dual<5>; 5] = std::array::from_fn(|k| Dual::variable(u[k], k));
    let r = residuals(&ud, Dual::constant(p), &b.map(Dual::constant), Dual::constant(h));
    std::array::from_fn(|i| r[i].eps)
}

fn newton_step(u: &[f64; 5], p: f64, b: &[f64; 3], h: f64) -> [f64; 5] {
    let r = residuals(u, p, b, h);
    let r_norm = norm(&r);
    let mut j = jacobian(u, p, b, h);
    for (k, row) in j.iter_mut().enumerate() {
        row[k] += 1.0e-12;
    }
    let du: [f64; 5] = solve(j, r.map(|x| -x));
    let caps = [3.0, 3.0, 3.0, 1.0, 500.0];
    let du: [f64; 5] = std::array::from_fn(|k| du[k].clamp(-caps[k], caps[k]));
    let alphas = [1.0, 0.5, 0.25, 0.125, 0.062_5, 0.031_25, 0.015_625];
    let mut last = *u;
    for alpha in alphas {
        let mut trial: [f64; 5] = std::array::from_fn(|k| u[k] + alpha * du[k]);
        trial[4] = trial[4].clamp(TMIN_CLAMP, TMAX_CLAMP);
        if norm(&residuals(&trial, p, b, h)) < r_norm {
            return trial;
        }
        last = trial;
    }
    last
}

fn initial_guess(p: f64, b: &[f64; 3], t_guess: f64) -> [f64; 5] {
    let (bc, bh, bo) = (b[0], b[1], b[2]);
    let n_co2 = 0.90 * bc;
    let n_co = 0.10 * bc;
    let n_h2o = 0.90 * bh / 2.0;
    let n_h2 = 0.05 * bh / 2.0;
    let n_oh = 0.05 * bh / 2.0;
    let n_h = 0.01 * bh / 2.0;
    let o_used = 2.0 * n_co2 + n_co + n_h2o + n_oh;
    let n_o2 = (0.5 * (bo - o_used)).max(1.0e-4);
    let n = [1.0e-8, n_o2, n_h2o, n_co2, n_co, n_h2, n_oh, n_h, 1.0e-4].map(|v: f64| v.max(1.0e-10));
    let n_tot: f64 = n.iter().sum();
    let mu = mu_over_rt(t_guess);
    let idx = [3, 2, 6];
    let rhs: [f64; 3] = std::array::from_fn(|k| n[idx[k]].ln() + mu[idx[k]] + (p / P_REF).ln() - n_tot.ln());
    let a: [[f64; 3]; 3] = std::array::from_fn(|k| ATOMS[idx[k]]);
    let pi: [f64; 3] = solve(a, rhs);
    [pi[0], pi[1], pi[2], n_tot.ln(), t_guess]
}

#[must_use]
pub fn solve_primal(p: f64, b: &[f64; 3], h_mixed: f64, t_guess: f64, max_iter: usize) -> ([f64; 5], f64) {
    let mut u = initial_guess(p, b, t_guess);
    for _ in 0..max_iter.max(1) {
        u = newton_step(&u, p, b, h_mixed);
    }
    let r = residuals(&u, p, b, h_mixed);
    (u, norm(&r))
}

fn parameter_residuals<S: Scalar>(u: &[S; 5], q: &[S; 5]) -> [S; 5] {
    residuals(u, q[0], &[q[1], q[2], q[3]], q[4])
}

fn mixed_residual(u: &[f64; 5], q: &[f64; 5], ua: &[f64; 5], ub: &[f64; 5], a: &[f64; 5], b: &[f64; 5]) -> [f64; 5] {
    let us = std::array::from_fn(|i| HyperDual::new(u[i], ua[i], ub[i], 0.0));
    let qs = std::array::from_fn(|i| HyperDual::new(q[i], a[i], b[i], 0.0));
    parameter_residuals(&us, &qs).map(|r| r.e12)
}

fn sensitivity_condition(j: [[f64; 5]; 5]) -> f64 {
    let mut inverse = [[0.0; 5]; 5];
    for k in 0..5 {
        let col = solve(j, std::array::from_fn(|i| if i == k { 1.0 } else { 0.0 }));
        for i in 0..5 { inverse[i][k] = col[i]; }
    }
    let norm = |m: [[f64; 5]; 5]| m.iter().map(|r| r.iter().map(|x| x.abs()).sum::<f64>()).fold(0.0, f64::max);
    if inverse.iter().flatten().any(|x| !x.is_finite()) { return f64::INFINITY; }
    norm(j) * norm(inverse)
}

pub fn solve_equilibrium<S: Scalar>(
    p: S, b: &[S; 3], h_mixed: S, t_guess: f64, max_iter: usize,
) -> ([S; 5], f64) {
    let bv = b.map(|x| x.value());
    let (u, rnorm) = solve_primal(p.value(), &bv, h_mixed.value(), t_guess, max_iter);
    let params = [p, b[0], b[1], b[2], h_mixed];
    let q = params.map(|v| v.value());
    let j = jacobian(&u, q[0], &bv, q[4]);
    if !sensitivity_condition(j).is_finite() {
        return (u.map(|v| S::lift(v, &params, &[f64::NAN; 5], &[f64::NAN; 25])), rnorm);
    }
    let uc = u.map(S::from_f64);
    let r = parameter_residuals(&uc, &params);
    let du = solve(j, r.map(|v| v - S::from_f64(v.value())));
    let first: [S; 5] = std::array::from_fn(|k| uc[k] - du[k]);
    let qd: [Dual<5>; 5] = std::array::from_fn(|i| Dual::variable(q[i], i));
    let rp = parameter_residuals(&u.map(Dual::constant), &qd);
    let mut g = [[0.0; 5]; 5];
    for i in 0..5 {
        let col = solve(j, rp.map(|r| -r.eps[i]));
        for k in 0..5 { g[k][i] = col[k]; }
    }
    let mut h = [[0.0; 25]; 5];
    let mut correction = [[0.0; 25]; 5];
    for i in 0..5 { for l in 0..5 {
        let a = std::array::from_fn(|k| if k == i { 1.0 } else { 0.0 });
        let b = std::array::from_fn(|k| if k == l { 1.0 } else { 0.0 });
        let gi = std::array::from_fn(|k| g[k][i]);
        let gl = std::array::from_fn(|k| g[k][l]);
        let full = mixed_residual(&u, &q, &gi, &gl, &a, &b);
        let fixed = mixed_residual(&u, &q, &[0.0; 5], &[0.0; 5], &a, &b);
        let hi = solve(j, full.map(|v| -v));
        let ci = solve(j, std::array::from_fn(|k| fixed[k] - full[k]));
        for k in 0..5 { h[k][i * 5 + l] = hi[k]; correction[k][i * 5 + l] = ci[k]; }
    }}
    let attached = std::array::from_fn(|component| {
        first[component] + S::lift3(0.0, &params, &[0.0; 5], &correction[component], |a, b| {
            let a: [f64; 5] = a.try_into().expect("five input directions");
            let b: [f64; 5] = b.try_into().expect("five input directions");
            let ga = std::array::from_fn(|k| (0..5).map(|i| g[k][i] * a[i]).sum());
            let gb = std::array::from_fn(|k| (0..5).map(|i| g[k][i] * b[i]).sum());
            let hab = std::array::from_fn(|k| (0..5).map(|i| (0..5).map(|l| h[k][i*5+l] * a[i] * b[l]).sum::<f64>()).sum());
            (0..5).map(|i| {
                let seed = Jet3::<1>::variable(0.0, 0, 0.0, 0.0);
                let qs = std::array::from_fn(|k| Jet3::directed(q[k], a[k], b[k]) + seed * if k == i { 1.0 } else { 0.0 });
                let us = std::array::from_fn(|k| Jet3::directed(u[k], ga[k], gb[k]) + seed * g[k][i]);
                let full = parameter_residuals(&us, &qs).map(|v| v.third()[0]);
                let fixed = parameter_residuals(&u.map(Jet3::constant), &qs).map(|v| v.third()[0]);
                let gi = std::array::from_fn(|k| g[k][i]);
                let ei = std::array::from_fn(|k| if k == i { 1.0 } else { 0.0 });
                let hia = std::array::from_fn(|k| (0..5).map(|l| h[k][i*5+l] * a[l]).sum());
                let hib = std::array::from_fn(|k| (0..5).map(|l| h[k][i*5+l] * b[l]).sum());
                let ra = mixed_residual(&u, &q, &gi, &hab, &ei, &[0.0; 5]);
                let rb = mixed_residual(&u, &q, &ga, &hib, &a, &[0.0; 5]);
                let rc = mixed_residual(&u, &q, &gb, &hia, &b, &[0.0; 5]);
                solve(j, std::array::from_fn(|k| -(full[k] - fixed[k] + ra[k] + rb[k] + rc[k])))[component]
            }).collect()
        })
    });
    (attached, rnorm)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EquilibriumState<S> {
    pub t_flame: S,
    pub y_products: [S; 9],
    pub gamma_effective: S,
    pub r_effective: S,
    pub cp_effective: S,
    pub dissociation_fraction: S,
    pub residual_norm: f64,
    pub n_total_per_kg: S,
}

impl EquilibriumState<f64> {
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"T_flame": self.t_flame, "y_products": self.y_products, "gamma_effective": self.gamma_effective,
               "R_effective": self.r_effective, "Cp_effective": self.cp_effective,
               "dissociation_fraction": self.dissociation_fraction, "residual_norm": self.residual_norm,
               "n_total_per_kg": self.n_total_per_kg})
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EquilibriumChemistrySolver {
    pub max_newton_iter: usize,
    pub convergence_tol: f64,
    pub switch_certificate: f64,
}

impl Default for EquilibriumChemistrySolver {
    fn default() -> Self {
        Self { max_newton_iter: 50, convergence_tol: 1.0e-6, switch_certificate: SWITCH_CERTIFICATE }
    }
}

fn shape_error(y_len: usize, with_species: bool) -> ModelError {
    let got = format!("({y_len},)");
    if with_species {
        ModelError::contract(format!(
            "y_reactants must have shape (2,) matching REACTANT_SPECIES=('CH4', 'O2'); got {got}"
        ))
    } else {
        ModelError::contract(format!("y_reactants must have shape (2,); got {got}"))
    }
}

fn admit_inputs(p: f64, y: [f64; 2], h: f64, t_guess: f64) -> PResult<()> {
    if !p.is_finite() || p <= 0.0 {
        return Err(ModelError::contract("equilibrium pressure must be positive and finite"));
    }
    if y.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0) || (y[0] + y[1] - 1.0).abs() > 1.0e-10 {
        return Err(ModelError::contract("reactant mass fractions must be finite, nonnegative and sum to one"));
    }
    if !h.is_finite() || !t_guess.is_finite() || t_guess <= 0.0 {
        return Err(ModelError::contract("equilibrium enthalpy must be finite and temperature guess positive and finite"));
    }
    Ok(())
}

impl EquilibriumChemistrySolver {

    pub fn new(max_newton_iter: i64, convergence_tol: f64, switch_certificate: f64) -> PResult<Self> {
        if max_newton_iter <= 0 {
            return Err(ModelError::contract("max_newton_iter must be positive"));
        }
        if convergence_tol <= 0.0 || !convergence_tol.is_finite() {
            return Err(ModelError::contract("convergence_tol must be positive and finite"));
        }
        if switch_certificate <= 0.0 || !switch_certificate.is_finite() {
            return Err(ModelError::contract("switch_certificate must be positive and finite"));
        }
        Ok(Self {
            max_newton_iter: usize::try_from(max_newton_iter).unwrap_or(usize::MAX),
            convergence_tol,
            switch_certificate,
        })
    }

    pub fn element_balance<S: Scalar>(y: [S; 2]) -> [S; 3] {
        let n = [y[0] / MW[0], y[1] / MW[1]];
        std::array::from_fn(|j| n[0] * ATOMS[0][j] + n[1] * ATOMS[1][j])
    }


    pub fn evaluate<S: Scalar>(
        &self,
        p: S,
        y: &[S],
        h_mixed: S,
        t_guess: f64,
    ) -> PResult<EquilibriumState<S>> {
        let y: [S; 2] = y.try_into().map_err(|_| shape_error(y.len(), true))?;
        admit_inputs(p.value(), y.map(|v| v.value()), h_mixed.value(), t_guess)?;
        Self::new(self.max_newton_iter.try_into().map_err(|_| ModelError::contract("iteration cap exceeds integer range"))?, self.convergence_tol, self.switch_certificate)?;
        let b = Self::element_balance(y);
        let (u, rnorm) = solve_equilibrium(p, &b, h_mixed, t_guess, self.max_newton_iter);
        if !rnorm.is_finite() || rnorm > self.convergence_tol || u.iter().any(|v| !v.value().is_finite()) {
            return Err(ModelError::contract("equilibrium solve exceeds the scaled residual tolerance"));
        }
        let n: [S; 9] = log_moles(&u, p).map(|x| x.clip(-80.0, 80.0).exp());
        let n_tot = sum(n);
        let mass: [S; 9] = std::array::from_fn(|i| n[i] * MW[i]);
        let total = sum(mass);
        let y_products = mass.map(|m| m / total);
        let n_per_kg = sum((0..9).map(|i| y_products[i] / MW[i]));
        let mw_mix = n_per_kg.recip();
        let r_eff = S::from_f64(RU) / mw_mix;
        let t = u[4];
        let cp = cp_over_r(t);
        let cp_eff = sum((0..9).map(|i| y_products[i] * (cp[i] * RU / MW[i])));
        let cv_eff = cp_eff - r_eff;
        let x = n.map(|v| v / n_tot);
        Ok(EquilibriumState {
            t_flame: t,
            y_products,
            gamma_effective: cp_eff / cv_eff,
            r_effective: r_eff,
            cp_effective: cp_eff,
            dissociation_fraction: x[6] + x[7] + x[8],
            residual_norm: rnorm,
            n_total_per_kg: n_tot,
        })
    }


    pub fn certify_sensitivity(&self, p: f64, y: &[f64], h_mixed: f64, t_guess: f64) -> PResult<Value> {
        let yy: [f64; 2] = y.try_into().map_err(|_| shape_error(y.len(), false))?;
        admit_inputs(p, yy, h_mixed, t_guess)?;
        Self::new(self.max_newton_iter.try_into().map_err(|_| ModelError::contract("iteration cap exceeds integer range"))?, self.convergence_tol, self.switch_certificate)?;
        let b = Self::element_balance(yy);
        let (u, rnorm) = solve_primal(p, &b, h_mixed, t_guess, self.max_newton_iter);
        if !rnorm.is_finite() || rnorm > self.convergence_tol {
            return Err(ModelError::contract(format!(
                "EquilibriumChemistrySolver Newton did not converge: scaled residual norm = {} at (p = {} Pa, y_reactants = ({}, {}), h_mixed = {} J/kg).  Reduce the step, supply a warmer T_guess, or move the reactants toward a chemically-reasonable OF window (2 <= OF_mass <= 6 for CH4/O2).",
                fmt_e(rnorm, 3),
                fmt_e(p, 3),
                repr_float(yy[0]),
                repr_float(yy[1]),
                fmt_e(h_mixed, 3)
            )));
        }
        let condition = sensitivity_condition(jacobian(&u, p, &b, h_mixed));
        let roundoff_estimate = condition * f64::EPSILON;
        let clipped_species = log_moles(&u, p).iter().any(|v| !v.is_finite() || v.abs() >= 80.0 - self.switch_certificate);
        if !roundoff_estimate.is_finite() || roundoff_estimate >= self.switch_certificate || clipped_species
            || u[4] <= TMIN_CLAMP || u[4] >= TMAX_CLAMP {
            return Err(ModelError::contract("equilibrium sensitivity requires a nonsingular, unclipped converged branch"));
        }
        Ok(json!({"jacobian_condition_inf": condition, "linear_roundoff_estimate": roundoff_estimate, "condition_basis": "native unknowns and scaled residuals",
                  "residual_norm": rnorm, "switch_certificate": self.switch_certificate,
                  "convergence_tol": self.convergence_tol, "sensitivity_admissible": true}))
    }
}
