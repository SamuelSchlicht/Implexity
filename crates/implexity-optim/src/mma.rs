// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::time::Instant;

use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseMatrix, solve};

use crate::numeric::format_e;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum MmaError {
    #[error("{0}")]
    Cae(#[from] CaeError),
    #[error("{0}")]
    Runtime(String),
}

pub type EvalTrue<'a> = &'a mut dyn FnMut(&[f64]) -> (f64, Vec<f64>);

pub type MmaResult<T> = Result<T, MmaError>;

#[derive(Debug, Clone, PartialEq)]
pub struct MmaStepInfo {
    pub iteration: i64,
    pub kkt_residual: f64,
    pub subproblem_kkt: f64,
    pub subproblem_converged: bool,
    pub subproblem_ip_residual: f64,
    pub dual_variables: Vec<f64>,
    pub y: Vec<f64>,
    pub z: f64,
    pub inner_iterations: i64,
    pub conservative: bool,
    pub rho: Option<Vec<f64>>,
    pub wallclock_s: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MmaSolveResult {
    pub x: Vec<f64>,
    pub f0: f64,
    pub iterations: i64,
    pub converged: bool,
    pub kkt_residual: f64,
    pub dual_variables: Vec<f64>,
    pub wallclock_s: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MmaStateDict {
    pub l: Option<Vec<f64>>,
    pub u: Option<Vec<f64>>,
    pub iter: Option<i64>,
    pub kkt: Option<f64>,
    pub lam: Option<Vec<f64>>,
    pub y: Option<Vec<f64>>,
    pub z: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MmaOptions {
    pub move_limit: f64,
    pub asymptote_init: f64,
    pub asymptote_expand: f64,
    pub asymptote_contract: f64,
    pub asymptote_lo: f64,
    pub asymptote_hi: f64,
    pub a0: f64,
    pub a: Option<Vec<f64>>,
    pub c: Option<Vec<f64>>,
    pub d: Option<Vec<f64>>,
    pub raa0: f64,
    pub albefa: f64,
    pub epsimin: f64,
    pub max_inner: i64,
    pub strict_subproblem: bool,
    pub x_min: Vec<f64>,
    pub x_max: Vec<f64>,
}

impl Default for MmaOptions {
    fn default() -> Self {
        Self {
            move_limit: 0.2,
            asymptote_init: 0.5,
            asymptote_expand: 1.2,
            asymptote_contract: 0.7,
            asymptote_lo: 0.01,
            asymptote_hi: 10.0,
            a0: 1.0,
            a: None,
            c: None,
            d: None,
            raa0: 1e-5,
            albefa: 0.1,
            epsimin: 1e-7,
            max_inner: 200,
            strict_subproblem: false,
            x_min: vec![0.0],
            x_max: vec![1.0],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GcmmaOptions {
    pub rho_min: f64,
    pub rho_init: f64,
    pub rho_growth: f64,
    pub max_conservative: i64,
}

impl Default for GcmmaOptions {
    fn default() -> Self {
        Self { rho_min: 1e-6, rho_init: 0.1, rho_growth: 10.0, max_conservative: 15 }
    }
}

fn check_finite(name: &str, values: &[f64]) -> MmaResult<()> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(MmaError::Runtime(format!("MMA: non-finite values in {name}")))
    }
}

fn broadcast(values: &[f64], n: usize, name: &str) -> CaeResult<Vec<f64>> {
    match values.len() {
        1 => Ok(vec![values[0]; n]),
        k if k == n => Ok(values.to_vec()),
        _ => Err(CaeError::contract(format!("MMA: {name} is not broadcastable to ({n},)"))),
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn matvec(a: &[f64], m: usize, n: usize, x: &[f64]) -> Vec<f64> {
    (0..m).map(|i| dot(&a[i * n..(i + 1) * n], x)).collect()
}

fn matvec_t(a: &[f64], m: usize, n: usize, y: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; n];
    for i in 0..m {
        for j in 0..n {
            out[j] += a[i * n + j] * y[i];
        }
    }
    out
}

fn max_of(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

struct Sub<'a> {
    p0: &'a [f64],
    q0: &'a [f64],
    p: &'a [f64],
    q: &'a [f64],
    alpha: &'a [f64],
    beta: &'a [f64],
    b: &'a [f64],
}

#[derive(Clone)]
struct Vars {
    x: Vec<f64>,
    y: Vec<f64>,
    z: f64,
    lam: Vec<f64>,
    xsi: Vec<f64>,
    eta: Vec<f64>,
    mu: Vec<f64>,
    zet: f64,
    s: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct MmaOptimizer {
    pub n: usize,
    pub m: usize,
    pub move_limit: f64,
    asymptote_init: f64,
    asymptote_expand: f64,
    asymptote_contract: f64,
    asymptote_lo: f64,
    asymptote_hi: f64,
    a0: f64,
    a: Vec<f64>,
    c: Vec<f64>,
    d: Vec<f64>,
    raa0: f64,
    albefa: f64,
    epsimin: f64,
    max_inner: i64,
    strict_subproblem: bool,
    x_min: Vec<f64>,
    x_max: Vec<f64>,
    xmami: Vec<f64>,
    pub l: Option<Vec<f64>>,
    pub u: Option<Vec<f64>>,
    iter: i64,
    last_kkt: f64,
    lam: Vec<f64>,
    y: Vec<f64>,
    z: f64,
    ip_converged: bool,
    ip_last_residual: f64,
    pub last: Option<MmaStepInfo>,
    gcmma: Option<GcmmaState>,
}

#[derive(Debug, Clone)]
struct GcmmaState {
    options: GcmmaOptions,
    nonfinite_trials: i64,
    rho0: Option<f64>,
    rho: Option<Vec<f64>>,
}

fn coefficient(value: Option<&Vec<f64>>, m: usize, default: f64, name: &str) -> CaeResult<Vec<f64>> {
    match value {
        None => Ok(vec![default; m]),
        Some(v) => {
            let arr = if v.len() == 1 { vec![v[0]; m] } else { v.clone() };
            if arr.len() != m {
                return Err(CaeError::contract(format!(
                    "MMA: coefficient '{name}' is not broadcastable to ({m},)"
                )));
            }
            if arr.iter().any(|x| !x.is_finite()) {
                return Err(CaeError::contract(format!("MMA: coefficient '{name}' must be finite")));
            }
            Ok(arr)
        }
    }
}

impl MmaOptimizer {

    pub fn new(design_dim: i64, n_constraints: i64, options: &MmaOptions) -> CaeResult<Self> {
        if design_dim < 1 {
            return Err(CaeError::contract("MMA: design_dim must be >= 1"));
        }
        if n_constraints < 0 {
            return Err(CaeError::contract("MMA: n_constraints must be >= 0"));
        }
        let n =
            usize::try_from(design_dim).map_err(|_| CaeError::contract("MMA: design_dim must be >= 1"))?;
        let m = usize::try_from(n_constraints)
            .map_err(|_| CaeError::contract("MMA: n_constraints must be >= 0"))?;
        let o = options;
        if !(0.0 < o.move_limit && o.move_limit <= 1.0) {
            return Err(CaeError::contract("MMA: move_limit must lie in (0, 1]"));
        }
        if !(0.0 < o.asymptote_lo && o.asymptote_lo < o.asymptote_init && o.asymptote_init <= o.asymptote_hi)
        {
            return Err(CaeError::contract("MMA: require 0 < asymptote_lo < asymptote_init <= asymptote_hi"));
        }
        let a = coefficient(o.a.as_ref(), m, 0.0, "a")?;
        let c = coefficient(o.c.as_ref(), m, 1000.0, "c")?;
        let d = coefficient(o.d.as_ref(), m, 1.0, "d")?;
        if !(0.0 < o.albefa && o.albefa < 1.0) {
            return Err(CaeError::contract("MMA: albefa must lie in (0, 1)"));
        }
        if o.epsimin <= 0.0 || o.max_inner < 1 {
            return Err(CaeError::contract("MMA: epsimin must be > 0 and max_inner >= 1"));
        }
        let x_min = broadcast(&o.x_min, n, "x_min")?;
        let x_max = broadcast(&o.x_max, n, "x_max")?;
        if x_max.iter().zip(&x_min).any(|(hi, lo)| hi <= lo) {
            return Err(CaeError::contract("MMA: x_max must exceed x_min in every coordinate"));
        }
        let xmami = x_max.iter().zip(&x_min).map(|(hi, lo)| f64::max(hi - lo, 1e-5)).collect();
        Ok(Self {
            n,
            m,
            move_limit: o.move_limit,
            asymptote_init: o.asymptote_init,
            asymptote_expand: o.asymptote_expand,
            asymptote_contract: o.asymptote_contract,
            asymptote_lo: o.asymptote_lo,
            asymptote_hi: o.asymptote_hi,
            a0: o.a0,
            a,
            c,
            d,
            raa0: o.raa0,
            albefa: o.albefa,
            epsimin: o.epsimin,
            max_inner: o.max_inner,
            strict_subproblem: o.strict_subproblem,
            x_min,
            x_max,
            xmami,
            l: None,
            u: None,
            iter: 0,
            last_kkt: f64::INFINITY,
            lam: vec![0.0; m],
            y: vec![0.0; m],
            z: 0.0,
            ip_converged: true,
            ip_last_residual: 0.0,
            last: None,
            gcmma: None,
        })
    }


    pub fn gcmma(
        design_dim: i64,
        n_constraints: i64,
        options: &MmaOptions,
        gcmma: &GcmmaOptions,
    ) -> CaeResult<Self> {
        let mut out = Self::new(design_dim, n_constraints, options)?;
        if gcmma.rho_min <= 0.0 || gcmma.rho_init <= 0.0 || gcmma.rho_growth <= 1.0 {
            return Err(CaeError::contract("GCMMA: require rho_min > 0, rho_init > 0, rho_growth > 1"));
        }
        if gcmma.max_conservative < 0 {
            return Err(CaeError::contract("GCMMA: max_conservative must be >= 0"));
        }
        out.gcmma = Some(GcmmaState { options: gcmma.clone(), nonfinite_trials: 0, rho0: None, rho: None });
        Ok(out)
    }

    #[must_use]
    pub fn is_gcmma(&self) -> bool {
        self.gcmma.is_some()
    }

    #[must_use]
    pub fn nonfinite_trials(&self) -> i64 {
        self.gcmma.as_ref().map_or(0, |g| g.nonfinite_trials)
    }

    #[must_use]
    pub fn converged(&self, tol: f64) -> bool {
        self.last_kkt < tol
    }

    #[must_use]
    pub fn dual_variables(&self) -> Vec<f64> {
        self.lam.clone()
    }

    #[must_use]
    pub fn state_dict(&self) -> MmaStateDict {
        MmaStateDict {
            l: self.l.clone(),
            u: self.u.clone(),
            iter: Some(self.iter),
            kkt: Some(self.last_kkt),
            lam: Some(self.lam.clone()),
            y: Some(self.y.clone()),
            z: Some(self.z),
        }
    }


    pub fn load_state_dict(&mut self, sd: &MmaStateDict) -> CaeResult<()> {
        if sd.l.is_some() != sd.u.is_some() {
            return Err(CaeError::contract("MMA: state_dict carries only one of L/U"));
        }
        if let (Some(l), Some(u)) = (&sd.l, &sd.u) {
            if l.len() != self.n || u.len() != self.n {
                return Err(CaeError::contract("MMA: state_dict L/U shape mismatch"));
            }
            if u.iter().zip(l).any(|(u, l)| u <= l) {
                return Err(CaeError::contract("MMA: state_dict asymptotes not ordered L < U"));
            }
        }
        let vec_or_zero = |v: &Option<Vec<f64>>, name: &str| -> CaeResult<Vec<f64>> {
            match v {
                None => Ok(vec![0.0; self.m]),
                Some(v) if v.len() == self.m => Ok(v.clone()),
                Some(_) => Err(CaeError::contract(format!("MMA: state_dict {name} shape mismatch"))),
            }
        };
        self.l.clone_from(&sd.l);
        self.u.clone_from(&sd.u);
        self.iter = sd.iter.unwrap_or(0);
        self.last_kkt = sd.kkt.unwrap_or(f64::INFINITY);
        self.lam = vec_or_zero(&sd.lam, "lam")?;
        self.y = vec_or_zero(&sd.y, "y")?;
        self.z = sd.z.unwrap_or(0.0);
        Ok(())
    }

    #[must_use]
    pub fn kkt_residual(
        &self,
        x: &[f64],
        df0dx: &[f64],
        fs: &[f64],
        dfsdx: &[f64],
        multipliers: Option<(&[f64], &[f64], f64)>,
    ) -> f64 {
        if self.iter == 0 && multipliers.is_none() {
            return f64::INFINITY;
        }
        let (lam, y, z) = multipliers.unwrap_or((&self.lam, &self.y, self.z));
        let grad: Vec<f64> = if self.m > 0 {
            let t = matvec_t(dfsdx, self.m, self.n, lam);
            df0dx.iter().zip(&t).map(|(a, b)| a + b).collect()
        } else {
            df0dx.to_vec()
        };
        let g: Vec<f64> = (0..self.m).map(|j| fs[j] - self.a[j] * z - y[j]).collect();
        self.natural_residual(x, y, z, lam, &grad, &g, &self.x_min, &self.x_max)
    }

    #[allow(clippy::too_many_arguments)]
    fn natural_residual(
        &self,
        x: &[f64],
        y: &[f64],
        z: f64,
        lam: &[f64],
        grad: &[f64],
        g: &[f64],
        lo: &[f64],
        hi: &[f64],
    ) -> f64 {
        let r_x =
            x.iter().zip(grad).zip(lo.iter().zip(hi)).map(|((x, g), (l, h))| x - (x - g).max(*l).min(*h));
        let r_y = (0..self.m).map(|j| y[j] - f64::max(0.0, y[j] - (self.c[j] + self.d[j] * y[j] - lam[j])));
        let r_z = z - f64::max(0.0, z - (self.a0 - dot(&self.a, lam)));
        let r_lam = (0..self.m).map(|j| lam[j] - f64::max(0.0, lam[j] + g[j]));
        let abs_max = |it: &mut dyn Iterator<Item = f64>| it.map(f64::abs).fold(0.0_f64, f64::max);
        let mut blocks = vec![abs_max(&mut r_x.into_iter()), r_z.abs()];
        if self.m > 0 {
            blocks.push(abs_max(&mut r_y.into_iter()));
            blocks.push(abs_max(&mut r_lam.into_iter()));
        }
        max_of(&blocks)
    }

    fn lu(&self) -> CaeResult<(&[f64], &[f64])> {
        match (&self.l, &self.u) {
            (Some(l), Some(u)) => Ok((l, u)),
            _ => Err(CaeError::contract(
                "MMA: asymptotes L/U are unset; call step() or load_state_dict() first",
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn coerce(
        &self,
        x_k: &[f64],
        x_km1: &[f64],
        x_km2: &[f64],
        f0: f64,
        df0dx: &[f64],
        fs: &[f64],
        dfsdx: &[f64],
    ) -> CaeResult<Vec<f64>> {
        let (n, m) = (self.n, self.m);
        if x_k.len() != n
            || x_km1.len() != n
            || x_km2.len() != n
            || df0dx.len() != n
            || fs.len() != m
            || dfsdx.len() != m * n
        {
            return Err(CaeError::contract(format!(
                "MMA: input shapes must be x:(N,), df0dx:(N,), fs:(m,), dfsdx:(m,N) with N={n}, m={m}: cannot reshape"
            )));
        }
        for (name, arr) in
            [("x_k", x_k), ("x_km1", x_km1), ("x_km2", x_km2), ("df0dx", df0dx), ("fs", fs), ("dfsdx", dfsdx)]
        {
            if arr.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract(format!("MMA: non-finite entries in {name}")));
            }
        }
        if !f0.is_finite() {
            return Err(CaeError::contract("MMA: objective value f0 is not finite"));
        }
        if x_k.iter().zip(&self.x_min).any(|(x, lo)| *x < lo - 1e-12)
            || x_k.iter().zip(&self.x_max).any(|(x, hi)| *x > hi + 1e-12)
        {
            return Err(CaeError::contract("MMA: x_k lies outside [x_min, x_max]"));
        }
        Ok(x_k
            .iter()
            .zip(self.x_min.iter().zip(&self.x_max))
            .map(|(x, (lo, hi))| x.max(*lo).min(*hi))
            .collect())
    }

    fn asymptote_update(&mut self, x_k: &[f64], x_km1: &[f64], x_km2: &[f64]) {
        let span: Vec<f64> = self.x_max.iter().zip(&self.x_min).map(|(h, l)| h - l).collect();
        let (Some(l), Some(u)) =
            (self.l.as_ref().filter(|_| self.iter >= 2), self.u.as_ref().filter(|_| self.iter >= 2))
        else {
            self.l = Some(x_k.iter().zip(&span).map(|(x, s)| x - self.asymptote_init * s).collect());
            self.u = Some(x_k.iter().zip(&span).map(|(x, s)| x + self.asymptote_init * s).collect());
            return;
        };
        let mut low = Vec::with_capacity(self.n);
        let mut upp = Vec::with_capacity(self.n);
        for i in 0..self.n {
            let zzz = (x_k[i] - x_km1[i]) * (x_km1[i] - x_km2[i]);
            let factor = if zzz > 0.0 {
                self.asymptote_expand
            } else if zzz < 0.0 {
                self.asymptote_contract
            } else {
                1.0
            };
            let mut lo = x_k[i] - factor * (x_km1[i] - l[i]);
            let mut up = x_k[i] + factor * (u[i] - x_km1[i]);
            lo = lo.max(x_k[i] - self.asymptote_hi * span[i]);
            lo = lo.min(x_k[i] - self.asymptote_lo * span[i]);
            up = up.min(x_k[i] + self.asymptote_hi * span[i]);
            up = up.max(x_k[i] + self.asymptote_lo * span[i]);
            low.push(lo);
            upp.push(up);
        }
        self.l = Some(low);
        self.u = Some(upp);
    }

    fn trust_box(&self, x_k: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let (l, u) = self.lu()?;
        let mut alpha = Vec::with_capacity(self.n);
        let mut beta = Vec::with_capacity(self.n);
        for i in 0..self.n {
            let span = self.x_max[i] - self.x_min[i];
            alpha.push(f64::max(
                f64::max(l[i] + self.albefa * (x_k[i] - l[i]), x_k[i] - self.move_limit * span),
                self.x_min[i],
            ));
            beta.push(f64::min(
                f64::min(u[i] - self.albefa * (u[i] - x_k[i]), x_k[i] + self.move_limit * span),
                self.x_max[i],
            ));
        }
        Ok((alpha, beta))
    }

    fn pq(&self, df: &[f64], x_k: &[f64], rho: f64) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let (l, u) = self.lu()?;
        let mut p = Vec::with_capacity(self.n);
        let mut q = Vec::with_capacity(self.n);
        for i in 0..self.n {
            let ux2 = (u[i] - x_k[i]).powi(2);
            let xl2 = (x_k[i] - l[i]).powi(2);
            let dfp = df[i].max(0.0);
            let dfm = (-df[i]).max(0.0);
            let pq = 0.001 * (dfp + dfm) + rho / self.xmami[i];
            p.push((dfp + pq) * ux2);
            q.push((dfm + pq) * xl2);
        }
        Ok((p, q))
    }

    fn big_pq(&self, dfs: &[f64], x_k: &[f64], rho: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let (l, u) = self.lu()?;
        let (m, n) = (self.m, self.n);
        let mut p = vec![0.0; m * n];
        let mut q = vec![0.0; m * n];
        for j in 0..m {
            for i in 0..n {
                let ux2 = (u[i] - x_k[i]).powi(2);
                let xl2 = (x_k[i] - l[i]).powi(2);
                let d = dfs[j * n + i];
                let dfp = d.max(0.0);
                let dfm = (-d).max(0.0);
                let pq = 0.001 * (dfp + dfm) + rho[j] / self.xmami[i];
                p[j * n + i] = (dfp + pq) * ux2;
                q[j * n + i] = (dfm + pq) * xl2;
            }
        }
        Ok((p, q))
    }

    fn inv_gaps(&self, x: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let (l, u) = self.lu()?;
        Ok((
            x.iter().zip(u).map(|(x, u)| 1.0 / (u - x)).collect(),
            x.iter().zip(l).map(|(x, l)| 1.0 / (x - l)).collect(),
        ))
    }

    fn rhs(&self, p: &[f64], q: &[f64], x_k: &[f64], fs: &[f64]) -> CaeResult<Vec<f64>> {
        let (ui, li) = self.inv_gaps(x_k)?;
        let a = matvec(p, self.m, self.n, &ui);
        let b = matvec(q, self.m, self.n, &li);
        Ok((0..self.m).map(|j| a[j] + b[j] - fs[j]).collect())
    }

    fn psi_rows(&self, p: &[f64], q: &[f64], rows: usize, x: &[f64]) -> CaeResult<Vec<f64>> {
        let (ui, li) = self.inv_gaps(x)?;
        let a = matvec(p, rows, self.n, &ui);
        let b = matvec(q, rows, self.n, &li);
        Ok(a.iter().zip(&b).map(|(a, b)| a + b).collect())
    }


    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        x_k: &[f64],
        x_km1: &[f64],
        x_km2: &[f64],
        f0: f64,
        df0dx: &[f64],
        fs: &[f64],
        dfsdx: &[f64],
    ) -> MmaResult<Vec<f64>> {
        if self.gcmma.is_some() {
            return Err(CaeError::contract(
                "GCMMA.step requires eval_true(x) -> (f0, fs) to test conservativeness of the approximation at the trial point",
            )
            .into());
        }
        let t0 = Instant::now();
        let x_k = self.coerce(x_k, x_km1, x_km2, f0, df0dx, fs, dfsdx)?;
        self.last_kkt = self.kkt_residual(&x_k, df0dx, fs, dfsdx, None);
        self.asymptote_update(&x_k, x_km1, x_km2);
        let (alpha, beta) = self.trust_box(&x_k)?;
        let (p0, q0) = self.pq(df0dx, &x_k, self.raa0)?;
        let (p, q) = self.big_pq(dfsdx, &x_k, &vec![self.raa0; self.m])?;
        let b = self.rhs(&p, &q, &x_k, fs)?;
        let sub = Sub { p0: &p0, q0: &q0, p: &p, q: &q, alpha: &alpha, beta: &beta, b: &b };
        let v = self.solve_subproblem(&sub)?;
        let sub_kkt = self.sub_kkt(&v, &sub)?;
        self.record(&v, sub_kkt, 0, true, None, t0);
        Ok(v.x)
    }


    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn step_gcmma(
        &mut self,
        x_k: &[f64],
        x_km1: &[f64],
        x_km2: &[f64],
        f0: f64,
        df0dx: &[f64],
        fs: &[f64],
        dfsdx: &[f64],
        eval_true: EvalTrue<'_>,
    ) -> MmaResult<Vec<f64>> {
        let Some(g) = self.gcmma.clone() else {
            return self.step(x_k, x_km1, x_km2, f0, df0dx, fs, dfsdx);
        };
        let opts = g.options;
        let t0 = Instant::now();
        let x_k = self.coerce(x_k, x_km1, x_km2, f0, df0dx, fs, dfsdx)?;
        self.last_kkt = self.kkt_residual(&x_k, df0dx, fs, dfsdx, None);
        self.asymptote_update(&x_k, x_km1, x_km2);
        let (alpha, beta) = self.trust_box(&x_k)?;
        let (n, m) = (self.n, self.m);
        let span: Vec<f64> = self.x_max.iter().zip(&self.x_min).map(|(h, l)| h - l).collect();
        let s0: f64 = df0dx.iter().zip(&span).map(|(d, s)| d.abs() * s).sum();
        let mut rho0 = f64::max(opts.rho_min, opts.rho_init * s0 / n as f64);
        let abs_dfs: Vec<f64> = dfsdx.iter().map(|v| v.abs()).collect();
        let mut rho: Vec<f64> = matvec(&abs_dfs, m, n, &span)
            .iter()
            .map(|r| f64::max(opts.rho_min, opts.rho_init * r / n as f64))
            .collect();
        let (uxinv_k, xlinv_k) = self.inv_gaps(&x_k)?;
        let mut inner = 0_i64;
        let mut conservative = false;
        let mut nonfinite = 0_i64;
        let (l, u) = {
            let (l, u) = self.lu()?;
            (l.to_vec(), u.to_vec())
        };
        let (v, p0, q0, p, q, b) = loop {
            let (p0, q0) = self.pq(df0dx, &x_k, rho0)?;
            let (p, q) = self.big_pq(dfsdx, &x_k, &rho)?;
            let b = self.rhs(&p, &q, &x_k, fs)?;
            let sub = Sub { p0: &p0, q0: &q0, p: &p, q: &q, alpha: &alpha, beta: &beta, b: &b };
            let v = self.solve_subproblem(&sub)?;
            let r0 = f0 - (dot(&p0, &uxinv_k) + dot(&q0, &xlinv_k));
            let f0app = r0 + self.psi_rows(&p0, &q0, 1, &v.x)?[0];
            let fapp: Vec<f64> = self.psi_rows(&p, &q, m, &v.x)?.iter().zip(&b).map(|(a, b)| a - b).collect();
            let (f0new, fnew) = eval_true(&v.x);
            if fnew.len() != m {
                return Err(CaeError::contract(format!(
                    "GCMMA: eval_true returned {} constraint values, expected {m}",
                    fnew.len()
                ))
                .into());
            }
            if !(f0new.is_finite() && fnew.iter().all(|v| v.is_finite())) {
                nonfinite += 1;
                if inner >= opts.max_conservative {
                    break (v, p0, q0, p, q, b);
                }
                inner += 1;
                rho0 *= opts.rho_growth;
                for r in &mut rho {
                    *r *= opts.rho_growth;
                }
                continue;
            }
            let bad0 = f0app + self.epsimin < f0new;
            let bad: Vec<bool> = fapp.iter().zip(&fnew).map(|(a, t)| a + self.epsimin < *t).collect();
            if !bad0 && !bad.iter().any(|b| *b) {
                conservative = true;
                break (v, p0, q0, p, q, b);
            }
            if inner >= opts.max_conservative {
                break (v, p0, q0, p, q, b);
            }
            inner += 1;
            let mut w = 0.0;
            for i in 0..n {
                let xxux = (v.x[i] - x_k[i]) / (u[i] - v.x[i]);
                let xxxl = (v.x[i] - x_k[i]) / (v.x[i] - l[i]);
                w += (xxux * xxxl) * ((u[i] - l[i]) / self.xmami[i]);
            }
            let w = f64::max(w, 1e-12);
            if bad0 {
                rho0 = f64::min(1.1 * (rho0 + (f0new - f0app) / w), opts.rho_growth * rho0);
            }
            for j in 0..m {
                if bad[j] {
                    rho[j] = f64::min(1.1 * (rho[j] + (fnew[j] - fapp[j]) / w), opts.rho_growth * rho[j]);
                }
            }
        };
        let sub = Sub { p0: &p0, q0: &q0, p: &p, q: &q, alpha: &alpha, beta: &beta, b: &b };
        let sub_kkt = self.sub_kkt(&v, &sub)?;
        if let Some(state) = &mut self.gcmma {
            state.nonfinite_trials += nonfinite;
            state.rho0 = Some(rho0);
            state.rho = Some(rho.clone());
        }
        let mut all_rho = vec![rho0];
        all_rho.extend(rho);
        self.record(&v, sub_kkt, inner, conservative, Some(all_rho), t0);
        Ok(v.x)
    }

    fn record(
        &mut self,
        v: &Vars,
        sub_kkt: f64,
        inner: i64,
        conservative: bool,
        rho: Option<Vec<f64>>,
        t0: Instant,
    ) {
        self.lam.clone_from(&v.lam);
        self.y.clone_from(&v.y);
        self.z = v.z;
        self.iter += 1;
        self.last = Some(MmaStepInfo {
            iteration: self.iter,
            kkt_residual: self.last_kkt,
            subproblem_kkt: sub_kkt,
            subproblem_converged: self.ip_converged,
            subproblem_ip_residual: self.ip_last_residual,
            dual_variables: self.lam.clone(),
            y: self.y.clone(),
            z: self.z,
            inner_iterations: inner,
            conservative,
            rho,
            wallclock_s: t0.elapsed().as_secs_f64(),
        });
    }


    #[allow(clippy::type_complexity)]
    pub fn solve(
        &mut self,
        x0: &[f64],
        fun: &mut dyn FnMut(&[f64]) -> (f64, Vec<f64>, Vec<f64>, Vec<f64>),
        max_iter: i64,
        tol: f64,
        x_tol: f64,
    ) -> MmaResult<MmaSolveResult> {
        let t0 = Instant::now();
        if x0.len() != self.n {
            return Err(CaeError::contract(format!("MMA: x0 must have shape ({},)", self.n)).into());
        }
        let mut x = x0.to_vec();
        let mut x_km1 = x.clone();
        let mut x_km2 = x.clone();
        let mut converged = false;
        let mut it = 0;
        let mut f0: Option<f64> = None;
        for k in 1..=max_iter {
            it = k;
            let (fv, df0dx, fs, dfsdx) = fun(&x);
            f0 = Some(fv);
            let x_new = if self.is_gcmma() {
                let mut eval_true = |xt: &[f64]| {
                    let (a, _, c, _) = fun(xt);
                    (a, c)
                };
                self.step_gcmma(&x, &x_km1, &x_km2, fv, &df0dx, &fs, &dfsdx, &mut eval_true)?
            } else {
                self.step(&x, &x_km1, &x_km2, fv, &df0dx, &fs, &dfsdx)?
            };
            if self.converged(tol) {
                converged = true;
                break;
            }
            let moved = x_new.iter().zip(&x).map(|(a, b)| (a - b).abs()).fold(0.0_f64, f64::max);
            x_km2 = x_km1;
            x_km1 = x;
            x = x_new;
            f0 = None;
            if x_tol > 0.0 && moved < x_tol {
                break;
            }
        }
        let f0 = match f0 {
            Some(v) => v,
            None => fun(&x).0,
        };
        Ok(MmaSolveResult {
            x,
            f0,
            iterations: it,
            converged,
            kkt_residual: self.last_kkt,
            dual_variables: self.lam.clone(),
            wallclock_s: t0.elapsed().as_secs_f64(),
        })
    }

    #[allow(clippy::too_many_lines)]
    fn ip_residual(&self, v: &Vars, sub: &Sub<'_>, epsi: f64) -> MmaResult<(f64, f64)> {
        let (l, u) = self.lu()?;
        let (n, m) = (self.n, self.m);
        let ux1: Vec<f64> = (0..n).map(|i| u[i] - v.x[i]).collect();
        let xl1: Vec<f64> = (0..n).map(|i| v.x[i] - l[i]).collect();
        let pt = matvec_t(sub.p, m, n, &v.lam);
        let qt = matvec_t(sub.q, m, n, &v.lam);
        let mut res = Vec::with_capacity(4 * n + 4 * m + 2);
        for i in 0..n {
            let plam = sub.p0[i] + pt[i];
            let qlam = sub.q0[i] + qt[i];
            res.push(plam / (ux1[i] * ux1[i]) - qlam / (xl1[i] * xl1[i]) - v.xsi[i] + v.eta[i]);
        }
        for j in 0..m {
            res.push(self.c[j] + self.d[j] * v.y[j] - v.mu[j] - v.lam[j]);
        }
        res.push(self.a0 - v.zet - dot(&self.a, &v.lam));
        let uinv: Vec<f64> = ux1.iter().map(|x| 1.0 / x).collect();
        let linv: Vec<f64> = xl1.iter().map(|x| 1.0 / x).collect();
        let a1 = matvec(sub.p, m, n, &uinv);
        let a2 = matvec(sub.q, m, n, &linv);
        for j in 0..m {
            res.push(a1[j] + a2[j] - self.a[j] * v.z - v.y[j] + v.s[j] - sub.b[j]);
        }
        for i in 0..n {
            res.push(v.xsi[i] * (v.x[i] - sub.alpha[i]) - epsi);
        }
        for i in 0..n {
            res.push(v.eta[i] * (sub.beta[i] - v.x[i]) - epsi);
        }
        for j in 0..m {
            res.push(v.mu[j] * v.y[j] - epsi);
        }
        res.push(v.zet * v.z - epsi);
        for j in 0..m {
            res.push(v.lam[j] * v.s[j] - epsi);
        }
        check_finite("interior-point residual", &res)?;
        let norm = res.iter().map(|r| r * r).sum::<f64>().sqrt();
        let maxabs = res.iter().map(|r| r.abs()).fold(0.0_f64, f64::max);
        Ok((norm, maxabs))
    }

    #[allow(clippy::too_many_lines, clippy::similar_names)]
    fn solve_subproblem(&mut self, sub: &Sub<'_>) -> MmaResult<Vars> {
        let (n, m) = (self.n, self.m);
        let (l, u) = {
            let (l, u) = self.lu()?;
            (l.to_vec(), u.to_vec())
        };
        if sub.beta.iter().zip(sub.alpha).any(|(b, a)| b <= a) {
            return Err(CaeError::contract("MMA: move-limit box has beta <= alpha").into());
        }
        if sub.alpha.iter().zip(&l).any(|(a, l)| a <= l) || sub.beta.iter().zip(&u).any(|(b, u)| b >= u) {
            return Err(CaeError::contract("MMA: move-limit box must lie strictly inside (L, U)").into());
        }
        if [sub.p0, sub.q0, sub.p, sub.q].iter().any(|a| a.iter().any(|v| *v < 0.0)) {
            return Err(CaeError::contract("MMA: p/q coefficients must be non-negative").into());
        }
        let x: Vec<f64> = sub.alpha.iter().zip(sub.beta).map(|(a, b)| 0.5 * (a + b)).collect();
        let mut v = Vars {
            xsi: x.iter().zip(sub.alpha).map(|(x, a)| f64::max(1.0 / (x - a), 1.0)).collect(),
            eta: x.iter().zip(sub.beta).map(|(x, b)| f64::max(1.0 / (b - x), 1.0)).collect(),
            x,
            y: vec![1.0; m],
            z: 1.0,
            lam: vec![1.0; m],
            mu: self.c.iter().map(|c| f64::max(1.0, 0.5 * c)).collect(),
            zet: 1.0,
            s: vec![1.0; m],
        };
        let mut epsi = 1.0_f64;
        let mut converged = true;
        let mut residumax = f64::INFINITY;
        while epsi > self.epsimin {
            let (mut residunorm, rm) = self.ip_residual(&v, sub, epsi)?;
            residumax = rm;
            let mut ittt = 0_i64;
            while residumax > 0.9 * epsi {
                if ittt >= self.max_inner {
                    if self.strict_subproblem {
                        return Err(CaeError::convergence(format!(
                            "MMA subproblem: interior-point iteration did not reach tolerance at barrier epsi={} within {} Newton steps (max|r|={})",
                            format_e(epsi, 1),
                            self.max_inner,
                            format_e(residumax, 3)
                        ))
                        .into());
                    }
                    converged = false;
                    break;
                }
                ittt += 1;
                let ux1: Vec<f64> = (0..n).map(|i| u[i] - v.x[i]).collect();
                let xl1: Vec<f64> = (0..n).map(|i| v.x[i] - l[i]).collect();
                let ux2: Vec<f64> = ux1.iter().map(|a| a * a).collect();
                let xl2: Vec<f64> = xl1.iter().map(|a| a * a).collect();
                let ux3: Vec<f64> = ux1.iter().zip(&ux2).map(|(a, b)| a * b).collect();
                let xl3: Vec<f64> = xl1.iter().zip(&xl2).map(|(a, b)| a * b).collect();
                let uxinv1: Vec<f64> = ux1.iter().map(|a| 1.0 / a).collect();
                let xlinv1: Vec<f64> = xl1.iter().map(|a| 1.0 / a).collect();
                let uxinv2: Vec<f64> = ux2.iter().map(|a| 1.0 / a).collect();
                let xlinv2: Vec<f64> = xl2.iter().map(|a| 1.0 / a).collect();
                let pt = matvec_t(sub.p, m, n, &v.lam);
                let qt = matvec_t(sub.q, m, n, &v.lam);
                let plam: Vec<f64> = (0..n).map(|i| sub.p0[i] + pt[i]).collect();
                let qlam: Vec<f64> = (0..n).map(|i| sub.q0[i] + qt[i]).collect();
                let g1 = matvec(sub.p, m, n, &uxinv1);
                let g2 = matvec(sub.q, m, n, &xlinv1);
                let gvec: Vec<f64> = g1.iter().zip(&g2).map(|(a, b)| a + b).collect();
                let mut gg = vec![0.0; m * n];
                for j in 0..m {
                    for i in 0..n {
                        gg[j * n + i] = sub.p[j * n + i] * uxinv2[i] - sub.q[j * n + i] * xlinv2[i];
                    }
                }
                let dpsidx: Vec<f64> = (0..n).map(|i| plam[i] * uxinv2[i] - qlam[i] * xlinv2[i]).collect();
                let delx: Vec<f64> = (0..n)
                    .map(|i| dpsidx[i] - epsi / (v.x[i] - sub.alpha[i]) + epsi / (sub.beta[i] - v.x[i]))
                    .collect();
                let dely: Vec<f64> =
                    (0..m).map(|j| self.c[j] + self.d[j] * v.y[j] - v.lam[j] - epsi / v.y[j]).collect();
                let delz = self.a0 - dot(&self.a, &v.lam) - epsi / v.z;
                let dellam: Vec<f64> =
                    (0..m).map(|j| gvec[j] - self.a[j] * v.z - v.y[j] - sub.b[j] + epsi / v.lam[j]).collect();
                let diagx: Vec<f64> = (0..n)
                    .map(|i| {
                        2.0 * (plam[i] / ux3[i] + qlam[i] / xl3[i])
                            + v.xsi[i] / (v.x[i] - sub.alpha[i])
                            + v.eta[i] / (sub.beta[i] - v.x[i])
                    })
                    .collect();
                let diagy: Vec<f64> = (0..m).map(|j| self.d[j] + v.mu[j] / v.y[j]).collect();
                let diaglam: Vec<f64> = (0..m).map(|j| v.s[j] / v.lam[j]).collect();
                let diaglamyi: Vec<f64> = (0..m).map(|j| diaglam[j] + 1.0 / diagy[j]).collect();
                let (dx, dz, dlam) = if m < n {
                    let gdx: Vec<f64> = delx.iter().zip(&diagx).map(|(a, b)| a / b).collect();
                    let ggdx = matvec(&gg, m, n, &gdx);
                    let mut bb: Vec<f64> = (0..m).map(|j| dellam[j] + dely[j] / diagy[j] - ggdx[j]).collect();
                    bb.push(delz);
                    let dim = m + 1;
                    let mut aa = vec![0.0; dim * dim];
                    for r in 0..m {
                        for c in 0..m {
                            let mut s = 0.0;
                            for i in 0..n {
                                s += (gg[r * n + i] / diagx[i]) * gg[c * n + i];
                            }
                            aa[r * dim + c] = s + if r == c { diaglamyi[r] } else { 0.0 };
                        }
                        aa[r * dim + m] = self.a[r];
                        aa[m * dim + r] = self.a[r];
                    }
                    aa[m * dim + m] = -v.zet / v.z;
                    let solut = dense_solve(aa, dim, &bb)?;
                    let dlam = solut[..m].to_vec();
                    let dz = solut[m];
                    let gtl = matvec_t(&gg, m, n, &dlam);
                    let dx: Vec<f64> = (0..n).map(|i| -delx[i] / diagx[i] - gtl[i] / diagx[i]).collect();
                    (dx, dz, dlam)
                } else {
                    let diaglamyiinv: Vec<f64> = diaglamyi.iter().map(|d| 1.0 / d).collect();
                    let dellamyi: Vec<f64> = (0..m).map(|j| dellam[j] + dely[j] / diagy[j]).collect();
                    let dim = n + 1;
                    let mut aa = vec![0.0; dim * dim];
                    for r in 0..n {
                        for c in 0..n {
                            let mut s = 0.0;
                            for j in 0..m {
                                s += (gg[j * n + r] * diaglamyiinv[j]) * gg[j * n + c];
                            }
                            aa[r * dim + c] = s + if r == c { diagx[r] } else { 0.0 };
                        }
                    }
                    let ad: Vec<f64> = (0..m).map(|j| self.a[j] * diaglamyiinv[j]).collect();
                    let azz = v.zet / v.z + dot(&self.a, &ad);
                    let axz: Vec<f64> = matvec_t(&gg, m, n, &ad).iter().map(|x| -x).collect();
                    let dd: Vec<f64> = (0..m).map(|j| dellamyi[j] * diaglamyiinv[j]).collect();
                    let gtd = matvec_t(&gg, m, n, &dd);
                    let bx: Vec<f64> = (0..n).map(|i| delx[i] + gtd[i]).collect();
                    let bz = delz - dot(&self.a, &dd);
                    for r in 0..n {
                        aa[r * dim + n] = axz[r];
                        aa[n * dim + r] = axz[r];
                    }
                    aa[n * dim + n] = azz;
                    let mut rhs: Vec<f64> = bx.iter().map(|x| -x).collect();
                    rhs.push(-bz);
                    let solut = dense_solve(aa, dim, &rhs)?;
                    let dx = solut[..n].to_vec();
                    let dz = solut[n];
                    let ggdx = matvec(&gg, m, n, &dx);
                    let dlam: Vec<f64> = (0..m)
                        .map(|j| {
                            ggdx[j] * diaglamyiinv[j] - dz * (self.a[j] * diaglamyiinv[j])
                                + dellamyi[j] * diaglamyiinv[j]
                        })
                        .collect();
                    (dx, dz, dlam)
                };
                let dy: Vec<f64> = (0..m).map(|j| -dely[j] / diagy[j] + dlam[j] / diagy[j]).collect();
                let dxsi: Vec<f64> = (0..n)
                    .map(|i| {
                        -v.xsi[i] + epsi / (v.x[i] - sub.alpha[i])
                            - (v.xsi[i] * dx[i]) / (v.x[i] - sub.alpha[i])
                    })
                    .collect();
                let deta: Vec<f64> = (0..n)
                    .map(|i| {
                        -v.eta[i]
                            + epsi / (sub.beta[i] - v.x[i])
                            + (v.eta[i] * dx[i]) / (sub.beta[i] - v.x[i])
                    })
                    .collect();
                let dmu: Vec<f64> =
                    (0..m).map(|j| -v.mu[j] + epsi / v.y[j] - (v.mu[j] * dy[j]) / v.y[j]).collect();
                let dzet = -v.zet + epsi / v.z - v.zet * dz / v.z;
                let ds: Vec<f64> =
                    (0..m).map(|j| -v.s[j] + epsi / v.lam[j] - (v.s[j] * dlam[j]) / v.lam[j]).collect();
                let mut all = Vec::new();
                for part in [&dx, &dy, &vec![dz], &dlam, &dxsi, &deta, &dmu, &vec![dzet], &ds] {
                    all.extend_from_slice(part);
                }
                check_finite("Newton direction", &all)?;
                let mut xx = Vec::new();
                for part in [&v.y, &vec![v.z], &v.lam, &v.xsi, &v.eta, &v.mu, &vec![v.zet], &v.s] {
                    xx.extend_from_slice(part);
                }
                let mut dxx = Vec::new();
                for part in [&dy, &vec![dz], &dlam, &dxsi, &deta, &dmu, &vec![dzet], &ds] {
                    dxx.extend_from_slice(part);
                }
                let stmxx = if xx.is_empty() {
                    0.0
                } else {
                    max_of(&xx.iter().zip(&dxx).map(|(x, d)| -1.01 * d / x).collect::<Vec<_>>())
                };
                let stmalfa =
                    max_of(&(0..n).map(|i| -1.01 * dx[i] / (v.x[i] - sub.alpha[i])).collect::<Vec<_>>());
                let stmbeta =
                    max_of(&(0..n).map(|i| 1.01 * dx[i] / (sub.beta[i] - v.x[i])).collect::<Vec<_>>());
                let stminv = f64::max(f64::max(f64::max(stmalfa, stmbeta), stmxx), 1.0);
                let mut steg = 1.0 / stminv;
                let old = v.clone();
                let mut itto = 0;
                let mut resinew = 2.0 * residunorm;
                while resinew > residunorm && itto < 50 {
                    itto += 1;
                    v.x = (0..n).map(|i| old.x[i] + steg * dx[i]).collect();
                    v.y = (0..m).map(|j| old.y[j] + steg * dy[j]).collect();
                    v.z = old.z + steg * dz;
                    v.lam = (0..m).map(|j| old.lam[j] + steg * dlam[j]).collect();
                    v.xsi = (0..n).map(|i| old.xsi[i] + steg * dxsi[i]).collect();
                    v.eta = (0..n).map(|i| old.eta[i] + steg * deta[i]).collect();
                    v.mu = (0..m).map(|j| old.mu[j] + steg * dmu[j]).collect();
                    v.zet = old.zet + steg * dzet;
                    v.s = (0..m).map(|j| old.s[j] + steg * ds[j]).collect();
                    let (rn, rm) = self.ip_residual(&v, sub, epsi)?;
                    resinew = rn;
                    residumax = rm;
                    steg *= 0.5;
                }
                residunorm = resinew;
            }
            epsi *= 0.1;
        }
        self.ip_converged = converged;
        self.ip_last_residual = residumax;
        Ok(v)
    }

    fn sub_kkt(&self, v: &Vars, sub: &Sub<'_>) -> MmaResult<f64> {
        let (l, u) = self.lu()?;
        let (n, m) = (self.n, self.m);
        let pt = matvec_t(sub.p, m, n, &v.lam);
        let qt = matvec_t(sub.q, m, n, &v.lam);
        let grad: Vec<f64> = (0..n)
            .map(|i| {
                (sub.p0[i] + pt[i]) / (u[i] - v.x[i]).powi(2) - (sub.q0[i] + qt[i]) / (v.x[i] - l[i]).powi(2)
            })
            .collect();
        let psi = self.psi_rows(sub.p, sub.q, m, &v.x)?;
        let g: Vec<f64> = (0..m).map(|j| psi[j] - self.a[j] * v.z - v.y[j] - sub.b[j]).collect();
        Ok(self.natural_residual(&v.x, &v.y, v.z, &v.lam, &grad, &g, sub.alpha, sub.beta))
    }
}

fn dense_solve(a: Vec<f64>, dim: usize, b: &[f64]) -> MmaResult<Vec<f64>> {
    let m = DenseMatrix::new(dim, dim, a).map_err(|e| MmaError::Runtime(e.to_string()))?;
    solve(&m, b, 1).map_err(|e| MmaError::Runtime(format!("Singular matrix: {e}")))
}
