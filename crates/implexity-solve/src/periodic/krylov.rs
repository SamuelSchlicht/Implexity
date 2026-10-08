// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use implexity_core::error::{CaeError, CaeResult};

pub type RightPreconditioner<'a> = &'a dyn Fn(&[f64]) -> CaeResult<Vec<f64>>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatchedGmresOptions {
    pub rtol: f64,
    pub atol: f64,
    pub restart: usize,
    pub max_products: usize,
}

impl BatchedGmresOptions {
    fn validate(&self) -> CaeResult<()> {
        if !(self.rtol.is_finite() && self.rtol >= 0.0 && self.atol.is_finite() && self.atol >= 0.0) {
            return Err(CaeError::contract("batched GMRES tolerances must be finite and nonnegative"));
        }
        if self.rtol == 0.0 && self.atol == 0.0 {
            return Err(CaeError::contract("batched GMRES needs a positive relative or absolute tolerance"));
        }
        if self.restart == 0 || self.max_products == 0 {
            return Err(CaeError::contract(
                "batched GMRES restart length and product budget must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BatchedGmresResult {
    pub solutions: Vec<Vec<f64>>,
    pub residuals: Vec<f64>,
    pub rhs_norms: Vec<f64>,
    pub tolerances: Vec<f64>,
    pub products: Vec<usize>,
    pub converged: Vec<bool>,
    pub rounds: usize,
}

impl BatchedGmresResult {
    #[must_use]
    pub fn all_converged(&self) -> bool {
        self.converged.iter().all(|c| *c)
    }

    #[must_use]
    pub fn worst_ratio(&self) -> f64 {
        self.residuals
            .iter()
            .zip(&self.tolerances)
            .map(|(r, t)| {
                if *t > 0.0 {
                    r / t
                } else if *r == 0.0 {
                    0.0
                } else {
                    f64::INFINITY
                }
            })
            .fold(0.0, f64::max)
    }
}

pub(crate) fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub(crate) fn norm(a: &[f64]) -> f64 {
    dot(a, a).sqrt()
}

struct Cycle {
    beta: f64,
    basis: Vec<Vec<f64>>,
    hessenberg: Vec<Vec<f64>>,
    triangular: Vec<Vec<f64>>,
    givens: Vec<(f64, f64)>,
    g: Vec<f64>,
    breakdown: bool,
}

enum Phase {
    Initial,
    Arnoldi(Box<Cycle>),
    Certify,
    Done,
}

struct System {
    b: Vec<f64>,
    x: Vec<f64>,
    bnorm: f64,
    tol: f64,
    products: usize,
    residual: f64,
    converged: bool,
    phase: Phase,
    pending: Option<Vec<f64>>,
}

fn givens(f: f64, g: f64) -> (f64, f64) {
    if g == 0.0 {
        (1.0, 0.0)
    } else if f == 0.0 {
        (0.0, g.signum())
    } else {
        let r = f.hypot(g);
        (f / r, g / r)
    }
}

impl System {
    fn start_cycle(&mut self, residual: &[f64], beta: f64, restart: usize) {
        let v0: Vec<f64> = residual.iter().map(|r| r / beta).collect();
        let mut g = vec![0.0; restart + 1];
        g[0] = beta;
        self.phase = Phase::Arnoldi(Box::new(Cycle {
            beta,
            basis: vec![v0],
            hessenberg: Vec::with_capacity(restart),
            triangular: Vec::with_capacity(restart),
            givens: Vec::with_capacity(restart),
            g,
            breakdown: false,
        }));
    }

    fn finish_cycle(
        &mut self,
        cycle: &Cycle,
        precondition: Option<RightPreconditioner<'_>>,
    ) -> CaeResult<Vec<f64>> {
        let j = cycle.triangular.len();
        let mut y = vec![0.0; j];
        for i in (0..j).rev() {
            let mut s = cycle.g[i];
            for (k, yk) in y.iter().enumerate().skip(i + 1) {
                s -= cycle.triangular[k][i] * yk;
            }
            let d = cycle.triangular[i][i];
            y[i] = if d == 0.0 { 0.0 } else { s / d };
        }
        let n = self.x.len();
        let mut update = vec![0.0; n];
        for (k, yk) in y.iter().enumerate() {
            for (u, v) in update.iter_mut().zip(&cycle.basis[k]) {
                *u += yk * v;
            }
        }
        let update = match precondition {
            Some(m) => m(&update)?,
            None => update,
        };
        for (xi, ui) in self.x.iter_mut().zip(&update) {
            *xi += ui;
        }
        let mut t = vec![0.0; j + 1];
        t[0] = cycle.beta;
        for (k, yk) in y.iter().enumerate() {
            for (i, h) in cycle.hessenberg[k].iter().enumerate() {
                t[i] -= h * yk;
            }
        }
        let mut r = vec![0.0; n];
        for (i, ti) in t.iter().enumerate() {
            if let Some(v) = cycle.basis.get(i) {
                for (ri, vi) in r.iter_mut().zip(v) {
                    *ri += ti * vi;
                }
            }
        }
        Ok(r)
    }
}




#[allow(clippy::too_many_lines)]
pub fn batched_gmres<F>(
    n: usize,
    rhs: &[Vec<f64>],
    guesses: Option<&[Vec<f64>]>,
    options: &BatchedGmresOptions,
    precondition: Option<RightPreconditioner<'_>>,
    mut apply: F,
) -> CaeResult<BatchedGmresResult>
where
    F: FnMut(&[Vec<f64>]) -> CaeResult<Vec<Vec<f64>>>,
{
    options.validate()?;
    let restart = options.restart.min(n.max(1));
    if let Some(g) = guesses
        && g.len() != rhs.len()
    {
        return Err(CaeError::contract(
            "batched GMRES received a different number of guesses and right-hand sides",
        ));
    }
    let mut systems = Vec::with_capacity(rhs.len());
    for (s, b) in rhs.iter().enumerate() {
        if b.len() != n || !b.iter().all(|v| v.is_finite()) {
            return Err(CaeError::contract(format!(
                "batched GMRES right-hand side {s} must be a finite vector of length {n}"
            )));
        }
        let x0 = match guesses {
            Some(g) => {
                if g[s].len() != n || !g[s].iter().all(|v| v.is_finite()) {
                    return Err(CaeError::contract(format!(
                        "batched GMRES guess {s} must be a finite vector of length {n}"
                    )));
                }
                g[s].clone()
            }
            None => vec![0.0; n],
        };
        let bnorm = norm(b);
        let tol = options.atol.max(options.rtol * bnorm);
        let zero_guess = x0.iter().all(|v| *v == 0.0);
        let mut system = System {
            b: b.clone(),
            x: x0,
            bnorm,
            tol,
            products: 0,
            residual: bnorm,
            converged: false,
            phase: Phase::Initial,
            pending: None,
        };
        if zero_guess {
            if bnorm <= tol {
                system.converged = true;
                system.phase = Phase::Done;
            } else {
                system.start_cycle(b, bnorm, restart);
            }
        }
        systems.push(system);
    }
    let mut rounds = 0usize;
    loop {

        let mut batch = Vec::new();
        let mut owners = Vec::new();
        for (s, system) in systems.iter_mut().enumerate() {
            if system.products >= options.max_products {
                if !matches!(system.phase, Phase::Done) {
                    system.phase = Phase::Done;
                }
                continue;
            }
            let vector = match &system.phase {
                Phase::Initial | Phase::Certify => Some(system.x.clone()),
                Phase::Arnoldi(cycle) => {
                    let v = &cycle.basis[cycle.basis.len() - 1];
                    Some(match precondition {
                        Some(m) => m(v)?,
                        None => v.clone(),
                    })
                }
                Phase::Done => None,
            };
            if let Some(v) = vector {
                system.pending = Some(v.clone());
                batch.push(v);
                owners.push(s);
            }
        }
        if batch.is_empty() {
            break;
        }
        rounds += 1;
        let products = apply(&batch)?;
        if products.len() != batch.len() {
            return Err(CaeError::contract("batched GMRES operator returned a different number of products"));
        }
        for (w, &s) in products.into_iter().zip(&owners) {
            if w.len() != n || !w.iter().all(|v| v.is_finite()) {
                return Err(CaeError::convergence(
                    "batched GMRES operator product is not a finite vector of the system order",
                ));
            }
            let system = &mut systems[s];
            system.products += 1;
            system.pending = None;
            let phase = std::mem::replace(&mut system.phase, Phase::Done);
            match phase {
                Phase::Initial | Phase::Certify => {
                    let r: Vec<f64> = system.b.iter().zip(&w).map(|(b, a)| b - a).collect();
                    let beta = norm(&r);
                    system.residual = beta;
                    if beta <= system.tol {
                        system.converged = true;
                        system.phase = Phase::Done;
                    } else {
                        system.converged = false;
                        system.start_cycle(&r, beta, restart);
                    }
                }
                Phase::Arnoldi(mut cycle) => {
                    let mut w = w;
                    let j = cycle.basis.len() - 1;
                    let mut h = vec![0.0; j + 2];
                    for _pass in 0..2 {
                        for (i, v) in cycle.basis.iter().enumerate() {
                            let c = dot(&w, v);
                            h[i] += c;
                            for (wk, vk) in w.iter_mut().zip(v) {
                                *wk -= c * vk;
                            }
                        }
                    }
                    let hn = norm(&w);
                    h[j + 1] = hn;
                    let scale = h.iter().map(|v| v.abs()).fold(0.0, f64::max);
                    if hn <= f64::EPSILON * scale || hn == 0.0 {
                        cycle.breakdown = true;
                    } else {
                        cycle.basis.push(w.iter().map(|v| v / hn).collect());
                    }
                    cycle.hessenberg.push(h.clone());
                    let mut col = h;
                    for (i, (c, s)) in cycle.givens.iter().enumerate() {
                        let (a, b) = (col[i], col[i + 1]);
                        col[i] = c * a + s * b;
                        col[i + 1] = -s * a + c * b;
                    }
                    let (c, s) = givens(col[j], col[j + 1]);
                    col[j] = c * col[j] + s * col[j + 1];
                    col[j + 1] = 0.0;
                    cycle.givens.push((c, s));
                    let gj = cycle.g[j];
                    cycle.g[j] = c * gj;
                    cycle.g[j + 1] = -s * gj;
                    cycle.triangular.push(col);
                    let estimate = cycle.g[j + 1].abs();
                    let steps = cycle.triangular.len();
                    if estimate <= system.tol || steps >= restart || cycle.breakdown {
                        let r = system.finish_cycle(&cycle, precondition)?;
                        system.residual = norm(&r).max(estimate);
                        system.phase = Phase::Certify;
                    } else {
                        system.phase = Phase::Arnoldi(cycle);
                    }
                }
                Phase::Done => {}
            }
        }
    }
    Ok(BatchedGmresResult {
        rhs_norms: systems.iter().map(|s| s.bnorm).collect(),
        tolerances: systems.iter().map(|s| s.tol).collect(),
        residuals: systems.iter().map(|s| s.residual).collect(),
        products: systems.iter().map(|s| s.products).collect(),
        converged: systems.iter().map(|s| s.converged).collect(),
        solutions: systems.into_iter().map(|s| s.x).collect(),
        rounds,
    })
}

