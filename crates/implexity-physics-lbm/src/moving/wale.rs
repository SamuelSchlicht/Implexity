// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

use super::boundary::Topology;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wale {
    pub constant: f64,
    pub denominator_floor: f64,
}

impl Wale {

    pub fn validate(&self) -> CaeResult<()> {
        if !(self.constant.is_finite()
            && self.constant > 0.0
            && self.denominator_floor.is_finite()
            && self.denominator_floor > 0.0)
        {
            return Err(CaeError::contract(
                "WALE constant and denominator_floor must be finite and positive",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Difference {
    Zero,
    Central(usize, usize),
    Forward(usize),
    Backward(usize),
}

#[derive(Clone, Debug)]
pub struct Stencil {
    pub diff: Vec<[Difference; 3]>,
}

impl Stencil {
    #[must_use]
    pub fn new<const Q: usize>(topology: &Topology<Q>) -> Self {
        let grid = topology.grid;
        let n = grid.cells();
        let diff = (0..n)
            .map(|x| {
                std::array::from_fn(|a| {
                    if topology.wall[x] || grid.shape[a] == 1 {
                        return Difference::Zero;
                    }
                    let mut o = [0i64; 3];
                    o[a] = 1;
                    let plus = grid
                        .inside(x, o, topology.periodic)
                        .then(|| grid.wrap(x, o))
                        .filter(|&y| !topology.wall[y]);
                    o[a] = -1;
                    let minus = grid
                        .inside(x, o, topology.periodic)
                        .then(|| grid.wrap(x, o))
                        .filter(|&y| !topology.wall[y]);
                    match (plus, minus) {
                        (Some(p), Some(m)) => Difference::Central(p, m),
                        (Some(p), None) => Difference::Forward(p),
                        (None, Some(m)) => Difference::Backward(m),
                        (None, None) => Difference::Zero,
                    }
                })
            })
            .collect();
        Self { diff }
    }

    #[must_use]
    pub fn gradient<S: Scalar>(&self, u: &[[S; 3]], x: usize) -> [[S; 3]; 3] {
        let mut g = [[S::zero(); 3]; 3];
        for b in 0..3 {
            for a in 0..3 {
                g[a][b] = match self.diff[x][b] {
                    Difference::Zero => S::zero(),
                    Difference::Central(p, m) => (u[p][a] - u[m][a]) * 0.5,
                    Difference::Forward(p) => u[p][a] - u[x][a],
                    Difference::Backward(m) => u[x][a] - u[m][a],
                };
            }
        }
        g
    }

    #[must_use]
    pub fn gradient_transpose<S: Scalar>(
        &self,
        g_bar: &[[[S; 3]; 3]],
        y: usize,
        neighbours: &[usize],
    ) -> [S; 3] {
        let mut out = [S::zero(); 3];
        let mut add = |x: usize, b: usize, coef: f64| {
            for a in 0..3 {
                out[a] += g_bar[x][a][b] * coef;
            }
        };
        for &x in neighbours.iter().chain(std::iter::once(&y)) {
            for b in 0..3 {
                match self.diff[x][b] {
                    Difference::Zero => {}
                    Difference::Central(p, m) => {
                        if p == y {
                            add(x, b, 0.5);
                        }
                        if m == y {
                            add(x, b, -0.5);
                        }
                    }
                    Difference::Forward(p) => {
                        if p == y {
                            add(x, b, 1.0);
                        }
                        if x == y {
                            add(x, b, -1.0);
                        }
                    }
                    Difference::Backward(m) => {
                        if x == y {
                            add(x, b, 1.0);
                        }
                        if m == y {
                            add(x, b, -1.0);
                        }
                    }
                }
            }
        }
        out
    }
}

fn invariants<S: Scalar>(g: &[[S; 3]; 3]) -> ([[S; 3]; 3], [[S; 3]; 3], S, S) {
    let mut g2 = [[S::zero(); 3]; 3];
    for a in 0..3 {
        for b in 0..3 {
            for c in 0..3 {
                g2[a][b] += g[a][c] * g[c][b];
            }
        }
    }
    let tr = (g2[0][0] + g2[1][1] + g2[2][2]) / 3.0;
    let mut sd = [[S::zero(); 3]; 3];
    let mut s = [[S::zero(); 3]; 3];
    let mut a_inv = S::zero();
    let mut b_inv = S::zero();
    for a in 0..3 {
        for b in 0..3 {
            sd[a][b] = (g2[a][b] + g2[b][a]) * 0.5 - if a == b { tr } else { S::zero() };
            s[a][b] = (g[a][b] + g[b][a]) * 0.5;
            a_inv += sd[a][b] * sd[a][b];
            b_inv += s[a][b] * s[a][b];
        }
    }
    (sd, s, a_inv, b_inv)
}

#[must_use]
pub fn omega<S: Scalar>(model: &Wale, g: &[[S; 3]; 3], tau0: S) -> S {
    let (_, _, a, b) = invariants(g);
    let num = a.powf(1.5);
    let den = b.powf(2.5) + a.powf(1.25) + model.denominator_floor;
    let nu = num / den * (model.constant * model.constant);
    S::one() / (tau0 + nu * 3.0)
}

#[must_use]
pub fn omega_vjp<S: Scalar>(model: &Wale, g: &[[S; 3]; 3], tau0: S, w: S) -> ([[S; 3]; 3], S) {
    let (sd, s, a, b) = invariants(g);
    let c2 = model.constant * model.constant;
    let num = a.powf(1.5);
    let den = b.powf(2.5) + a.powf(1.25) + model.denominator_floor;
    let nu = num / den * c2;
    let tau = tau0 + nu * 3.0;
    let tau_bar = -w / (tau * tau);
    let nu_bar = tau_bar * 3.0;
    let a_bar = nu_bar * c2 * (a.powf(0.5) * 1.5 / den - num * a.powf(0.25) * 1.25 / (den * den));
    let b_bar = -nu_bar * c2 * num * b.powf(1.5) * 2.5 / (den * den);
    let mut g_bar = [[S::zero(); 3]; 3];
    for x in 0..3 {
        for y in 0..3 {
            let mut da = S::zero();
            for k in 0..3 {
                da += sd[x][k] * g[y][k] + g[k][x] * sd[k][y];
            }
            g_bar[x][y] = a_bar * da * 2.0 + b_bar * s[x][y] * 2.0;
        }
    }
    (g_bar, tau_bar)
}

