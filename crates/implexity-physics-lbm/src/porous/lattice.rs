// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

use super::sp::{Sp, triplets};

pub const Q: usize = 27;
pub const CS2: f64 = 1.0 / 3.0;

pub const C: [[i32; 3]; Q] = {
    let mut out = [[0; 3]; Q];
    let mut i = 0;
    while i < Q {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let k = i as i32;
        out[i] = [k / 9 - 1, (k / 3) % 3 - 1, k % 3 - 1];
        i += 1;
    }
    out
};

pub const W: [f64; Q] = {
    let mut out = [0.0; Q];
    let mut i = 0;
    while i < Q {
        let mut w = 1.0;
        let mut a = 0;
        while a < 3 {
            w *= if C[i][a] == 0 { 2.0 / 3.0 } else { 1.0 / 6.0 };
            a += 1;
        }
        out[i] = w;
        i += 1;
    }
    out
};

pub const OPPOSITE: [usize; Q] = {
    let mut out = [0; Q];
    let mut i = 0;
    while i < Q {
        out[i] = Q - 1 - i;
        i += 1;
    }
    out
};

pub const POSITIVE: [usize; 13] = [
    13 + 1,
    13 + 2,
    13 + 3,
    13 + 4,
    13 + 5,
    13 + 6,
    13 + 7,
    13 + 8,
    13 + 9,
    13 + 10,
    13 + 11,
    13 + 12,
    13 + 13,
];

#[must_use]
pub fn cf(i: usize) -> [f64; 3] {
    C[i].map(f64::from)
}

#[must_use]
pub fn ii(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

#[must_use]
pub fn uu(v: i64) -> usize {
    usize::try_from(v).unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub n: [usize; 3],
}

impl Grid {
    #[must_use]
    pub fn cells(&self) -> usize {
        self.n[0] * self.n[1] * self.n[2]
    }

    #[must_use]
    pub fn id(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.n[1] + j) * self.n[2] + k
    }

    #[must_use]
    pub fn ijk(&self, c: usize) -> [usize; 3] {
        let k = c % self.n[2];
        let j = (c / self.n[2]) % self.n[1];
        [c / (self.n[1] * self.n[2]), j, k]
    }

    #[must_use]
    pub fn wrap(&self, c: usize, offset: [i64; 3]) -> usize {
        let p = self.ijk(c);
        let w = |a: usize| {
            let n = ii(self.n[a]);
            uu(((ii(p[a]) + offset[a]) % n + n) % n)
        };
        self.id(w(0), w(1), w(2))
    }

    #[must_use]
    pub fn inside_x(&self, c: usize, dx: i32) -> bool {
        let i = ii(self.ijk(c)[0]) + i64::from(dx);
        i >= 0 && i < ii(self.n[0])
    }

    #[must_use]
    pub fn plane(&self, index: usize) -> Vec<usize> {
        (0..self.n[1])
            .flat_map(|j| (0..self.n[2]).map(move |k| (j, k)))
            .map(|(j, k)| self.id(index, j, k))
            .collect()
    }

    #[must_use]
    pub fn halo(&self) -> Grid {
        Grid { n: self.n.map(|v| v + 2) }
    }
}

fn dot3<S: Scalar>(u: &[S; 3], c: [f64; 3]) -> S {
    u[0] * c[0] + u[1] * c[1] + u[2] * c[2]
}

#[must_use]
pub fn equilibrium<S: Scalar>(m: S, u: &[S; 3]) -> [S; Q] {
    let speed2 = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
    std::array::from_fn(|i| {
        let cu = dot3(u, cf(i));
        m * W[i] * ((cu / CS2 + 1.0) + (cu * cu - speed2 * CS2) / (2.0 * CS2 * CS2))
    })
}

#[must_use]
pub fn forcing<S: Scalar>(u: &[S; 3], force: &[S; 3], tau: S) -> [S; Q] {
    let pre = S::one() - S::from_f64(0.5) / tau;
    std::array::from_fn(|i| {
        let c = cf(i);
        let cu = dot3(u, c);
        let mut s = S::zero();
        for a in 0..3 {
            let term = (S::from_f64(c[a]) - u[a]) / CS2 + cu * c[a] / (CS2 * CS2);
            s += term * force[a];
        }
        pre * W[i] * s
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Exchange<S> {
    pub intrinsic_velocity: [S; 3],
    pub fluid_drag_force_density: [S; 3],
    pub solid_reaction_force_density: [S; 3],
    pub total_fluid_force_density: [S; 3],
    pub relative_work_dissipation: S,
    pub fluid_drag_work: S,
    pub solid_drag_work: S,
}

#[must_use]
pub fn exchange<S: Scalar>(m: S, momentum: &[S; 3], other: &[S; 3], us: &[S; 3], beta: S) -> Exchange<S> {
    let den = m + beta * 0.5;
    let velocity: [S; 3] = std::array::from_fn(|a| (momentum[a] + other[a] * 0.5 + beta * us[a] * 0.5) / den);
    let relative: [S; 3] = std::array::from_fn(|a| velocity[a] - us[a]);
    let fluid: [S; 3] = std::array::from_fn(|a| -(beta * relative[a]));
    let solid: [S; 3] = std::array::from_fn(|a| -fluid[a]);
    let rel2 = relative[0] * relative[0] + relative[1] * relative[1] + relative[2] * relative[2];
    Exchange {
        intrinsic_velocity: velocity,
        fluid_drag_force_density: fluid,
        solid_reaction_force_density: solid,
        total_fluid_force_density: std::array::from_fn(|a| other[a] + fluid[a]),
        relative_work_dissipation: beta * rel2,
        fluid_drag_work: fluid[0] * velocity[0] + fluid[1] * velocity[1] + fluid[2] * velocity[2],
        solid_drag_work: solid[0] * us[0] + solid[1] * us[1] + solid[2] * us[2],
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Collision<S> {
    pub drag: Exchange<S>,
    pub post: [S; Q],
    pub population_mass: S,
    pub pressure: S,
    pub pressure_correction: [S; 3],
}

#[must_use]
pub fn collision<S: Scalar>(f: &[S], q: S, g: &[S; 3], tau: S, us: &[S; 3], beta: S) -> Collision<S> {
    let m = f.iter().fold(S::zero(), |acc, v| acc + *v);
    let pressure = m * CS2 / q;
    let correction: [S; 3] = std::array::from_fn(|a| pressure * g[a]);
    let momentum: [S; 3] =
        std::array::from_fn(|a| (0..Q).fold(S::zero(), |acc, i| acc + f[i] * f64::from(C[i][a])));

    let other: [S; 3] = std::array::from_fn(|a| correction[a] + 0.0);
    let drag = exchange(m, &momentum, &other, us, beta);
    let eq = equilibrium(m, &drag.intrinsic_velocity);
    let fo = forcing(&drag.intrinsic_velocity, &drag.total_fluid_force_density, tau);
    let post = std::array::from_fn(|i| f[i] + (eq[i] - f[i]) / tau + fo[i]);
    Collision { drag, post, population_mass: m, pressure, pressure_correction: correction }
}

#[must_use]
pub fn physical_scales(rho: f64, h: f64, dt: f64) -> (f64, f64) {
    (rho * h.powi(4) / dt.powi(2), rho * h.powi(5) / dt.powi(3))
}

#[must_use]
pub fn quadrature_and_gradient<S: Scalar>(grid: Grid, halo: &[S]) -> (Vec<S>, Vec<[S; 3]>) {
    let h = grid.halo();
    let mut q = Vec::with_capacity(grid.cells());
    let mut g = Vec::with_capacity(grid.cells());
    for c in 0..grid.cells() {
        let [i, j, k] = grid.ijk(c);
        let at = |di: i32, dj: i32, dk: i32| {
            let idx = h.id(
                uu(ii(i) + 1 + i64::from(di)),
                uu(ii(j) + 1 + i64::from(dj)),
                uu(ii(k) + 1 + i64::from(dk)),
            );
            halo[idx]
        };
        let mut v = at(0, 0, 0) / 6.0;
        let mut grad = [S::zero(); 3];
        for (axis, gr) in grad.iter_mut().enumerate() {
            let mut lo = [0; 3];
            let mut hi = [0; 3];
            lo[axis] = -1;
            hi[axis] = 1;
            let a = at(lo[0], lo[1], lo[2]);
            let b = at(hi[0], hi[1], hi[2]);
            v += (a + b) * (5.0 / 36.0);
            *gr = (b - a) / 2.0;
        }
        q.push(v);
        g.push(grad);
    }
    (q, g)
}


pub fn quadrature_gradient_maps(grid: Grid) -> CaeResult<(Sp, Sp)> {
    let h = grid.halo();
    let nc = grid.cells();
    let nh = h.cells();
    let cells: Vec<[usize; 3]> = (0..nc).map(|c| grid.ijk(c).map(|v| v + 1)).collect();
    let (mut qr, mut qc, mut qv) = (Vec::new(), Vec::new(), Vec::new());
    let (mut gr, mut gc, mut gv) = (Vec::new(), Vec::new(), Vec::new());
    for (c, p) in cells.iter().enumerate() {
        qr.push(c);
        qc.push(h.id(p[0], p[1], p[2]));
        qv.push(1.0 / 6.0);
    }
    for axis in 0..3 {
        for sign in [-1_i64, 1] {
            for (c, p) in cells.iter().enumerate() {
                let mut nb = p.map(ii);
                nb[axis] += sign;
                let col = h.id(uu(nb[0]), uu(nb[1]), uu(nb[2]));
                qr.push(c);
                qc.push(col);
                qv.push(5.0 / 36.0);
                gr.push(3 * c + axis);
                gc.push(col);
                #[allow(clippy::cast_precision_loss)]
                gv.push(sign as f64 / 2.0);
            }
        }
    }
    Ok((triplets(nc, nh, &qr, &qc, &qv)?, triplets(3 * nc, nh, &gr, &gc, &gv)?))
}

#[must_use]
pub fn stream_open<S: Scalar>(grid: Grid, post: &[S]) -> (Vec<S>, Vec<S>) {
    let nc = grid.cells();
    let mut streamed = vec![S::zero(); nc * Q];
    let mut escaped = vec![S::zero(); nc * Q];
    for x in 0..nc {
        for i in 0..Q {
            let c = C[i];
            if grid.inside_x(x, -c[0]) {
                let src = grid.wrap(x, [-i64::from(c[0]), -i64::from(c[1]), -i64::from(c[2])]);
                streamed[x * Q + i] = post[src * Q + i];
            }
            if !grid.inside_x(x, c[0]) {
                escaped[x * Q + i] = post[x * Q + i];
            }
        }
    }
    (streamed, escaped)
}

#[must_use]
pub fn interior_rates<S: Scalar>(grid: Grid, post: &[S], scale: f64) -> Vec<Vec<S>> {
    let nc = grid.cells();
    POSITIVE
        .iter()
        .map(|&i| {
            let c = C[i];
            (0..nc)
                .map(|x| {
                    if grid.inside_x(x, c[0]) {
                        let nb = grid.wrap(x, [i64::from(c[0]), i64::from(c[1]), i64::from(c[2])]);
                        (post[x * Q + i] - post[nb * Q + OPPOSITE[i]]) * scale
                    } else {
                        S::zero()
                    }
                })
                .collect()
        })
        .collect()
}


pub fn transport_maps(grid: Grid, scale: f64) -> CaeResult<(Sp, Sp, Vec<Sp>)> {
    let nc = grid.cells();
    let size = nc * Q;
    let (mut sr, mut sc) = (Vec::new(), Vec::new());
    let (mut er, mut ec) = (Vec::new(), Vec::new());
    for x in 0..nc {
        for i in 0..Q {
            let c = C[i];
            if grid.inside_x(x, -c[0]) {
                let src = grid.wrap(x, [-i64::from(c[0]), -i64::from(c[1]), -i64::from(c[2])]);
                sr.push(x * Q + i);
                sc.push(src * Q + i);
            }
            if !grid.inside_x(x, c[0]) {
                er.push(x * Q + i);
                ec.push(x * Q + i);
            }
        }
    }
    let streamed = triplets(size, size, &sr, &sc, &vec![1.0; sr.len()])?;
    let escaped = triplets(size, size, &er, &ec, &vec![1.0; er.len()])?;
    let mut rates = Vec::with_capacity(13);
    for &i in &POSITIVE {
        let c = C[i];
        let rows: Vec<usize> = (0..nc).filter(|x| grid.inside_x(*x, c[0])).collect();
        let nbs: Vec<usize> =
            rows.iter().map(|x| grid.wrap(*x, [i64::from(c[0]), i64::from(c[1]), i64::from(c[2])])).collect();
        let mut r = rows.clone();
        r.extend(&rows);
        let mut cols: Vec<usize> = rows.iter().map(|x| Q * x + i).collect();
        cols.extend(nbs.iter().map(|x| Q * x + OPPOSITE[i]));
        let mut vals = vec![scale; rows.len()];
        vals.extend(vec![-scale; rows.len()]);
        rates.push(triplets(nc, size, &r, &cols, &vals)?);
    }
    Ok((streamed, escaped, rates))
}

#[derive(Clone, Debug)]
pub struct PortGeometry {
    pub axis: usize,
    pub side: i32,
    pub incoming: Vec<usize>,
    pub known: Vec<usize>,
    pub outgoing: Vec<usize>,
    pub tangent: Vec<usize>,
    pub axes: [usize; 2],
    pub b: [[f64; 9]; 3],
    pub correction: [[f64; 3]; 9],
}

fn inv3(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let cof = |r: usize, c: usize| {
        let rows: Vec<usize> = (0..3).filter(|v| *v != r).collect();
        let cols: Vec<usize> = (0..3).filter(|v| *v != c).collect();
        let minor = m[rows[0]][cols[0]] * m[rows[1]][cols[1]] - m[rows[0]][cols[1]] * m[rows[1]][cols[0]];
        if (r + c).is_multiple_of(2) { minor } else { -minor }
    };
    std::array::from_fn(|i| std::array::from_fn(|j| cof(j, i) / det))
}

impl PortGeometry {

    pub fn new(axis: usize, side: i32) -> CaeResult<Self> {
        if axis > 2 {
            return Err(CaeError::contract("axis must be integer 0, 1 or 2"));
        }
        if side != 1 && side != -1 {
            return Err(CaeError::contract("side must be integer inward-normal sign"));
        }
        let normal: Vec<i32> = (0..Q).map(|i| side * C[i][axis]).collect();
        let incoming: Vec<usize> = (0..Q).filter(|i| normal[*i] == 1).collect();
        let known: Vec<usize> = (0..Q).filter(|i| normal[*i] != 1).collect();
        let outgoing: Vec<usize> = (0..Q).filter(|i| normal[*i] == -1).collect();
        let tangent: Vec<usize> = (0..Q).filter(|i| normal[*i] == 0).collect();
        let others: Vec<usize> = (0..3).filter(|a| *a != axis).collect();
        let axes = [others[0], others[1]];
        let mut b = [[0.0; 9]; 3];
        for (k, &i) in incoming.iter().enumerate() {
            b[0][k] = 1.0;
            b[1][k] = f64::from(C[i][axes[0]]);
            b[2][k] = f64::from(C[i][axes[1]]);
        }
        let wb: [[f64; 3]; 9] = std::array::from_fn(|k| std::array::from_fn(|r| W[incoming[k]] * b[r][k]));
        let bwb: [[f64; 3]; 3] =
            std::array::from_fn(|r| std::array::from_fn(|c| (0..9).map(|k| b[r][k] * wb[k][c]).sum()));
        let inv = inv3(bwb);
        let correction: [[f64; 3]; 9] =
            std::array::from_fn(|k| std::array::from_fn(|c| (0..3).map(|r| wb[k][r] * inv[r][c]).sum()));
        Ok(Self { axis, side, incoming, known, outgoing, tangent, axes, b, correction })
    }
}

#[must_use]
pub fn pressure_port<S: Scalar>(
    geo: &PortGeometry,
    f: &[S],
    p: S,
    q: S,
    g: &[S; 3],
    external: &[S; 3],
    tangential: &[S; 2],
) -> [S; Q] {
    let side = f64::from(geo.side);
    let m = q * p / CS2;
    let force: [S; 3] = std::array::from_fn(|a| p * g[a] + external[a]);
    let sum = |set: &[usize]| set.iter().fold(S::zero(), |acc, i| acc + f[*i]);
    let jn = m - sum(&geo.tangent) - sum(&geo.outgoing) * 2.0;
    let un = (jn + force[geo.axis] * (0.5 * side)) / m;
    let mut velocity = [S::zero(); 3];
    velocity[geo.axis] = un * side;
    velocity[geo.axes[0]] = tangential[0];
    velocity[geo.axes[1]] = tangential[1];
    let eq = equilibrium(m, &velocity);
    let predictor: Vec<S> = geo
        .incoming
        .iter()
        .map(|&i| {
            let cf_i = cf(i);
            let cforce = force[0] * cf_i[0] + force[1] * cf_i[1] + force[2] * cf_i[2];
            f[OPPOSITE[i]] + eq[i] - eq[OPPOSITE[i]] - cforce * W[i] / CS2
        })
        .collect();
    let jt: [S; 2] = std::array::from_fn(|t| m * velocity[geo.axes[t]] - force[geo.axes[t]] * 0.5);
    let known_jt: [S; 2] = std::array::from_fn(|t| {
        geo.known.iter().fold(S::zero(), |acc, &i| acc + f[i] * f64::from(C[i][geo.axes[t]]))
    });
    let target = [m - sum(&geo.known), jt[0] - known_jt[0], jt[1] - known_jt[1]];
    let defect: [S; 3] = std::array::from_fn(|r| {
        target[r] - (0..9).fold(S::zero(), |acc, k| acc + predictor[k] * geo.b[r][k])
    });
    let mut out: [S; Q] = std::array::from_fn(|i| f[i]);
    for (k, &i) in geo.incoming.iter().enumerate() {
        out[i] = predictor[k]
            + (defect[0] * geo.correction[k][0]
                + defect[1] * geo.correction[k][1]
                + defect[2] * geo.correction[k][2]);
    }
    out
}

