// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::OnceLock;

use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lattice {
    D3q343Weighted,
    D3q39Unweighted,
    D3q39UnweightedM20,
    D3q39UnweightedM26,
    D3q343WeightedM26,
}

impl Lattice {

    pub fn parse(name: &str) -> CaeResult<Self> {
        Ok(match name {
            "d3q343_weighted" => Self::D3q343Weighted,
            "d3q39_unweighted" => Self::D3q39Unweighted,
            "d3q39_unweighted_m20" => Self::D3q39UnweightedM20,
            "d3q39_unweighted_m26" => Self::D3q39UnweightedM26,
            "d3q343_weighted_m26" => Self::D3q343WeightedM26,
            _ => return Err(CaeError::contract("Unknown compressible lattice formulation")),
        })
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::D3q343Weighted => "d3q343_weighted",
            Self::D3q39Unweighted => "d3q39_unweighted",
            Self::D3q39UnweightedM20 => "d3q39_unweighted_m20",
            Self::D3q39UnweightedM26 => "d3q39_unweighted_m26",
            Self::D3q343WeightedM26 => "d3q343_weighted_m26",
        }
    }

    #[must_use]
    pub fn weighted(self) -> bool {
        matches!(self, Self::D3q343Weighted | Self::D3q343WeightedM26)
    }

    fn moments(self) -> Moments {
        match self {
            Self::D3q343Weighted | Self::D3q39Unweighted => Moments::Heat,
            Self::D3q39UnweightedM20 => Moments::Third,
            Self::D3q39UnweightedM26 | Self::D3q343WeightedM26 => Moments::Fourth,
        }
    }

    #[must_use]
    pub fn data(self) -> &'static LatticeData {
        static TABLES: OnceLock<[LatticeData; 5]> = OnceLock::new();
        let tables = TABLES.get_or_init(|| {
            [
                LatticeData::build(full_velocities(), Moments::Heat),
                LatticeData::build(velocities39(), Moments::Heat),
                LatticeData::build(velocities39(), Moments::Third),
                LatticeData::build(velocities39(), Moments::Fourth),
                LatticeData::build(full_velocities(), Moments::Fourth),
            ]
        });
        match self {
            Self::D3q343Weighted => &tables[0],
            Self::D3q39Unweighted => &tables[1],
            Self::D3q39UnweightedM20 => &tables[2],
            Self::D3q39UnweightedM26 => &tables[3],
            Self::D3q343WeightedM26 => &tables[4],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Moments {
    Heat,
    Third,
    Fourth,
}

pub const SECOND: [(usize, usize); 6] = [(0, 0), (1, 1), (2, 2), (0, 1), (1, 2), (0, 2)];
pub const THIRD: [(usize, usize, usize); 10] = [
    (0, 0, 0),
    (0, 0, 1),
    (0, 0, 2),
    (0, 1, 1),
    (0, 1, 2),
    (0, 2, 2),
    (1, 1, 1),
    (1, 1, 2),
    (1, 2, 2),
    (2, 2, 2),
];

pub const SHELL_INVERSE: [[f64; 4]; 4] = [
    [1.0, -1.361_111_111_111_111_2, 0.388_888_888_888_888_84, -0.027_777_777_777_777_776],
    [0.0, 1.5, -0.541_666_666_666_666_6, 0.041_666_666_666_666_664],
    [0.0, -0.15, 0.166_666_666_666_666_66, -0.016_666_666_666_666_666],
    [-0.0, 0.011_111_111_111_111_112, -0.013_888_888_888_888_888, 0.002_777_777_777_777_778],
];

fn full_velocities() -> Vec<[i32; 3]> {
    let mut out = Vec::with_capacity(343);
    for a in -3..=3 {
        for b in -3..=3 {
            for c in -3..=3 {
                out.push([a, b, c]);
            }
        }
    }
    out
}

fn velocities39() -> Vec<[i32; 3]> {
    let allowed: [[i32; 3]; 6] = [[0, 0, 0], [0, 0, 1], [1, 1, 1], [0, 0, 2], [0, 2, 2], [0, 0, 3]];
    full_velocities()
        .into_iter()
        .filter(|c| {
            let mut s = c.map(i32::abs);
            s.sort_unstable();
            allowed.contains(&s)
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct LatticeData {
    pub velocities: Vec<[i32; 3]>,
    pub features: Vec<f64>,
    pub m: usize,
    pub speed2: Vec<f64>,
}

impl LatticeData {
    fn build(velocities: Vec<[i32; 3]>, moments: Moments) -> Self {
        let mut rows: Vec<Vec<f64>> = Vec::with_capacity(velocities.len());
        for c in &velocities {
            let cf = c.map(f64::from);
            let s2 = c[0] * c[0] + c[1] * c[1] + c[2] * c[2];
            let mut row: Vec<f64> = cf.iter().map(|v| v / 3.0).collect();
            for (a, b) in SECOND {
                row.push(f64::from(c[a] * c[b]) / 9.0);
            }
            match moments {
                Moments::Heat => row.extend(c.iter().map(|v| f64::from(v * s2) / 81.0)),
                Moments::Third | Moments::Fourth => {
                    for (a, b, d) in THIRD {
                        row.push(f64::from(c[a] * c[b] * c[d]) / 27.0);
                    }
                    if moments == Moments::Fourth {
                        for (a, b) in SECOND {
                            row.push(f64::from(c[a] * c[b] * s2) / 81.0);
                        }
                    }
                }
            }
            rows.push(row);
        }
        let m = rows.first().map_or(0, Vec::len);
        let speed2 = velocities.iter().map(|c| f64::from(c[0] * c[0] + c[1] * c[1] + c[2] * c[2])).collect();
        Self { velocities, features: rows.concat(), m, speed2 }
    }

    #[must_use]
    pub fn q(&self) -> usize {
        self.velocities.len()
    }
}

pub fn weights<S: Scalar>(temperature: S, data: &LatticeData) -> Vec<S> {
    let t = temperature;
    let v = [S::one(), t, t * 3.0 * t, t * 15.0 * t * t];
    let shells: [S; 4] = std::array::from_fn(|i| {
        let mut acc = v[0] * SHELL_INVERSE[i][0];
        for j in 1..4 {
            acc += v[j] * SHELL_INVERSE[i][j];
        }
        acc
    });
    let one = [shells[0], shells[1] / 2.0, shells[2] / 2.0, shells[3] / 2.0];
    data.velocities
        .iter()
        .map(|c| {
            let [a, b, d] = c.map(|v| one[v.unsigned_abs() as usize]);
            a * b * d
        })
        .collect()
}

pub fn target_moments<S: Scalar>(u: [S; 3], t: S, lattice: Lattice) -> Vec<S> {
    let mut out: Vec<S> = u.iter().map(|v| *v / 3.0).collect();
    for (a, b) in SECOND {
        let base = u[a] * u[b];
        out.push(if a == b { base + t } else { base } / 9.0);
    }
    let speed2 = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
    match lattice.moments() {
        Moments::Heat => out.extend(u.iter().map(|v| *v * (speed2 + t * 5.0) / 81.0)),
        Moments::Third | Moments::Fourth => {
            for (a, b, c) in THIRD {
                let pick = |cond: bool, v: S| if cond { v } else { S::zero() };
                let inner = pick(b == c, u[a]) + pick(a == c, u[b]) + pick(a == b, u[c]);
                out.push((u[a] * u[b] * u[c] + t * inner) / 27.0);
            }
            if lattice.moments() == Moments::Fourth {
                for (a, b) in SECOND {
                    let base = u[a] * u[b] * (speed2 + t * 7.0);
                    let extra = if a == b { t * speed2 + t * 5.0 * t } else { S::zero() };
                    out.push((base + extra) / 81.0);
                }
            }
        }
    }
    out
}

fn log_prior<S: Scalar>(temperature: S, data: &LatticeData, lattice: Lattice) -> Vec<S> {
    if lattice.weighted() {
        weights(temperature, data).into_iter().map(Scalar::ln).collect()
    } else {
        let q = data.q() as f64;
        vec![S::from_f64((1.0 / q).ln()); data.q()]
    }
}

fn logits(logw: &[f64], a: &[f64], m: usize, lam: &[f64]) -> Vec<f64> {
    logw.iter()
        .enumerate()
        .map(|(i, w)| {
            let mut z = 0.0;
            for k in 0..m {
                z += a[i * m + k] * lam[k];
            }
            w + z
        })
        .collect()
}

fn softmax(z: &[f64]) -> Vec<f64> {
    let top = z.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b));
    let e: Vec<f64> = z.iter().map(|v| (v - top).exp()).collect();
    let s: f64 = e.iter().sum();
    e.iter().map(|v| v / s).collect()
}

fn logsumexp(z: &[f64]) -> f64 {
    let top = z.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b));
    if !top.is_finite() {
        return top;
    }
    top + z.iter().map(|v| (v - top).exp()).sum::<f64>().ln()
}

fn mean_and_hessian(p: &[f64], a: &[f64], m: usize) -> (Vec<f64>, Vec<f64>) {
    let q = p.len();
    let mut mean = vec![0.0; m];
    for i in 0..q {
        for k in 0..m {
            mean[k] += p[i] * a[i * m + k];
        }
    }
    let mut h = vec![0.0; m * m];
    let mut centered = vec![0.0; m];
    for i in 0..q {
        for k in 0..m {
            centered[k] = a[i * m + k] - mean[k];
        }
        for r in 0..m {
            let w = centered[r] * p[i];
            for c in 0..m {
                h[r * m + c] += w * centered[c];
            }
        }
    }
    (mean, h)
}

fn solve(mut a: Vec<f64>, mut b: Vec<f64>, n: usize) -> Vec<f64> {
    let mut perm: Vec<usize> = (0..n).collect();
    for k in 0..n {
        let mut piv = k;
        for r in k + 1..n {
            if a[r * n + k].abs() > a[piv * n + k].abs() {
                piv = r;
            }
        }
        if piv != k {
            for c in 0..n {
                a.swap(k * n + c, piv * n + c);
            }
            b.swap(k, piv);
            perm.swap(k, piv);
        }
        let d = a[k * n + k];
        for r in k + 1..n {
            let l = a[r * n + k] / d;
            a[r * n + k] = l;
            for c in k + 1..n {
                a[r * n + c] -= l * a[k * n + c];
            }
            b[r] -= l * b[k];
        }
    }
    let mut x = vec![0.0; n];
    for k in (0..n).rev() {
        let mut s = b[k];
        for c in k + 1..n {
            s -= a[k * n + c] * x[c];
        }
        x[k] = s / a[k * n + k];
    }
    x
}

#[must_use]
pub fn multipliers(logw: &[f64], data: &LatticeData, target: &[f64]) -> Vec<f64> {
    let m = data.m;
    let a = &data.features;
    let dual = |lam: &[f64]| {
        let z = logits(logw, a, m, lam);
        logsumexp(&z) - lam.iter().zip(target).map(|(l, t)| l * t).sum::<f64>()
    };
    let error_of = |lam: &[f64]| {
        let p = softmax(&logits(logw, a, m, lam));
        let (mean, _) = mean_and_hessian(&p, a, m);
        mean.iter().zip(target).map(|(x, t)| x - t).collect::<Vec<f64>>()
    };
    let mut lam = vec![0.0; m];
    let mut count = 0;
    loop {
        let error = error_of(&lam);
        if count >= 60 || error.iter().fold(0.0_f64, |x, e| x.max(e.abs())) < 1e-13 {
            break;
        }
        let p = softmax(&logits(logw, a, m, &lam));
        let (_, h) = mean_and_hessian(&p, a, m);
        let direction = solve(h, error.clone(), m);
        let old = dual(&lam);
        let decrement: f64 = error.iter().zip(&direction).map(|(e, d)| e * d).sum();
        let acceptable = |scale: f64| {
            let trial: Vec<f64> = lam.iter().zip(&direction).map(|(l, d)| l - scale * d).collect();
            let v = dual(&trial);
            v.is_finite() && v <= old - 1e-4 * scale * decrement + 1e-15
        };
        let mut halvings = 0;
        let mut scale = 1.0;
        while halvings < 24 && !acceptable(scale) {
            halvings += 1;
            scale *= 0.5;
        }
        for (l, d) in lam.iter_mut().zip(&direction) {
            *l -= scale * d;
        }
        count += 1;
    }
    lam
}

pub fn equilibrium<S: Scalar>(rho: S, u: [S; 3], t: S, gamma: f64, lattice: Lattice) -> (Vec<S>, Vec<S>) {
    let data = lattice.data();
    let m = data.m;
    let a = &data.features;

    let uv = u.map(|v| v.value());
    let tv = t.value();
    let logw_v: Vec<f64> = log_prior(tv, data, lattice);
    let target_v: Vec<f64> = target_moments(uv, tv, lattice);
    let lam = multipliers(&logw_v, data, &target_v);
    let seeds = [
        Dual::<4>::new(uv[0], [1.0, 0.0, 0.0, 0.0]),
        Dual::new(uv[1], [0.0, 1.0, 0.0, 0.0]),
        Dual::new(uv[2], [0.0, 0.0, 1.0, 0.0]),
    ];
    let td = Dual::<4>::new(tv, [0.0, 0.0, 0.0, 1.0]);
    let target_d = target_moments(seeds, td, lattice);
    let logw_d = log_prior(td, data, lattice);
    let p = softmax(&logits(&logw_v, a, m, &lam));
    let (mean, h) = mean_and_hessian(&p, a, m);
    let mut grad = vec![[0.0; 4]; m];
    for k in 0..4 {
        let mut rhs: Vec<f64> = target_d.iter().map(|d| d.eps[k]).collect();
        for i in 0..p.len() {
            let w = p[i] * logw_d[i].eps[k];
            if w != 0.0 {
                for c in 0..m {
                    rhs[c] -= (a[i * m + c] - mean[c]) * w;
                }
            }
        }
        let d = solve(h.clone(), rhs, m);
        for c in 0..m {
            grad[c][k] = d[c];
        }
    }
    let inputs = [u[0], u[1], u[2], t];
    let lam_s: Vec<S> = (0..m).map(|c| S::lift(lam[c], &inputs, &grad[c], &[0.0; 16])).collect();
    let logw: Vec<S> = log_prior(t, data, lattice);
    let z: Vec<S> = (0..data.q())
        .map(|i| {
            let mut acc = S::zero();
            for c in 0..m {
                acc += lam_s[c] * a[i * m + c];
            }
            logw[i] + acc
        })
        .collect();
    let top = z.iter().fold(f64::NEG_INFINITY, |x, v| x.max(v.value()));
    let e: Vec<S> = z.iter().map(|v| (*v - top).exp()).collect();
    let mut total = e[0];
    for v in &e[1..] {
        total += *v;
    }
    let internal = (2.0 / (gamma - 1.0) - 3.0).max(0.0);
    let f: Vec<S> = e.iter().map(|v| rho * (*v / total)).collect();
    let g: Vec<S> = f.iter().map(|v| t * internal * *v).collect();
    (f, g)
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckedEquilibrium {
    pub f: Vec<f64>,
    pub g: Vec<f64>,
    pub maximum_scaled_moment_residual: f64,
}


pub fn checked_equilibrium(
    rho: f64,
    u: [f64; 3],
    t: f64,
    gamma: f64,
    lattice: Lattice,
) -> CaeResult<CheckedEquilibrium> {
    if !u.iter().all(|v| f64::is_finite(*v)) {
        return Err(CaeError::contract("Finite three-component lattice velocity required"));
    }
    if ![rho, t, gamma].iter().all(|v| f64::is_finite(*v)) {
        return Err(CaeError::contract("Finite real gas parameters required"));
    }
    if rho <= 0.0 || t <= 0.0 || !(1.0 < gamma && gamma <= 5.0 / 3.0) {
        return Err(CaeError::contract("Positive density/temperature and 1 < gamma <= 5/3 required"));
    }
    let data = lattice.data();
    if lattice.weighted() {
        let w = weights(t, data);
        if w.iter().any(|v| *v <= 0.0 || !v.is_finite()) {
            return Err(CaeError::contract("Temperature lies outside the positive multispeed-weight domain"));
        }
    }
    if u.iter().any(|v| v.abs() >= 3.0) {
        return Err(CaeError::contract("Velocity lies outside the fixed velocity support"));
    }
    let (f, g) = equilibrium(rho, u, t, gamma, lattice);
    let target = target_moments(u, t, lattice);
    let m = data.m;
    let mut error: f64 = 0.0;
    for (c, tc) in target.iter().enumerate() {
        let mut s = 0.0;
        for (i, fi) in f.iter().enumerate() {
            s += fi * data.features[i * m + c];
        }
        error = error.max((s / rho - tc).abs());
    }
    if !f.iter().all(|v| f64::is_finite(*v))
        || f.iter().any(|v| *v <= 0.0)
        || !g.iter().all(|v| f64::is_finite(*v))
        || g.iter().any(|v| *v < 0.0)
        || error > 1e-10
    {
        return Err(CaeError::contract(
            "Guided equilibrium is unrealizable or its nonlinear moment solve did not converge",
        ));
    }
    Ok(CheckedEquilibrium { f, g, maximum_scaled_moment_residual: error })
}

pub const EQUILIBRIUM_QUALIFICATION: &str = "local equilibrium admission only: moment residual, positivity and realizability are checked here; collision, transport and reservoir/wall operators carry their own local checks, while resolved flow, inlet/outlet and shock accuracy remain unqualified";

pub fn macroscopic<S: Scalar>(f: &[S], g: &[S], gamma: f64, data: &LatticeData) -> (S, [S; 3], S, S) {
    let mut rho = S::zero();
    let mut m = [S::zero(); 3];
    let mut kinetic = S::zero();
    let mut internal = S::zero();
    for (i, fi) in f.iter().enumerate() {
        rho += *fi;
        let c = data.velocities[i];
        for d in 0..3 {
            if c[d] != 0 {
                m[d] += *fi * f64::from(c[d]);
            }
        }
        kinetic += *fi * data.speed2[i];
        internal += g[i];
    }
    let u = [m[0] / rho, m[1] / rho, m[2] / rho];
    let energy = (kinetic + internal) * 0.5;
    let temperature = (energy / rho - (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]) * 0.5) * (gamma - 1.0);
    (rho, u, temperature, energy)
}

#[must_use]
pub fn moment_residual(f: &[f64], rho: f64, u: [f64; 3], t: f64, lattice: Lattice) -> f64 {
    let data = lattice.data();
    let target = target_moments(u, t, lattice);
    let m = data.m;
    let mut error: f64 = 0.0;
    for (c, tc) in target.iter().enumerate() {
        let mut s = 0.0;
        for (i, fi) in f.iter().enumerate() {
            s += fi * data.features[i * m + c];
        }
        error = error.max((s / rho - tc).abs());
    }
    error
}
