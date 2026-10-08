// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;

use crate::hyperelastic::kinematics::Mat3;

const TAYLOR_GAP: f64 = 1e-3;

#[must_use]
pub fn symmetric_eigen(m: &Mat3<f64>) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut a = *m;
    for i in 0..3 {
        for j in 0..i {
            let s = 0.5 * (a[i][j] + a[j][i]);
            a[i][j] = s;
            a[j][i] = s;
        }
    }
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let scale = a.iter().flatten().map(|x| x * x).sum::<f64>().sqrt();
    for _sweep in 0..64 {
        let off = a[0][1] * a[0][1] + a[0][2] * a[0][2] + a[1][2] * a[1][2];
        if off.sqrt() <= f64::EPSILON * 1e-2 * scale || off == 0.0 {
            break;
        }
        for (p, q) in [(0usize, 1usize), (0, 2), (1, 2)] {
            let apq = a[p][q];
            if apq == 0.0 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * apq);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for k in 0..3 {
                let (akp, akq) = (a[k][p], a[k][q]);
                a[k][p] = c * akp - s * akq;
                a[k][q] = s * akp + c * akq;
            }
            for k in 0..3 {
                let (apk, aqk) = (a[p][k], a[q][k]);
                a[p][k] = c * apk - s * aqk;
                a[q][k] = s * apk + c * aqk;
            }
            for row in &mut v {
                let (vp, vq) = (row[p], row[q]);
                row[p] = c * vp - s * vq;
                row[q] = s * vp + c * vq;
            }
        }
    }
    let values = [a[0][0], a[1][1], a[2][2]];

    let vectors = core::array::from_fn(|k| [v[0][k], v[1][k], v[2][k]]);
    (values, vectors)
}

pub trait SpectralScalar {
    fn eval(&self, c: f64) -> (f64, f64, f64, f64);
}

#[derive(Debug, Clone, Copy)]
pub struct Power(pub f64);

impl SpectralScalar for Power {
    fn eval(&self, c: f64) -> (f64, f64, f64, f64) {
        let m = self.0;
        (
            c.powf(m),
            m * c.powf(m - 1.0),
            m * (m - 1.0) * c.powf(m - 2.0),
            m * (m - 1.0) * (m - 2.0) * (m - 3.0) * c.powf(m - 4.0),
        )
    }
}

const PAIRS: [(usize, usize); 6] = [(0, 0), (1, 1), (2, 2), (1, 2), (0, 2), (0, 1)];

fn project(a: usize, n: &[f64; 3], m: &[f64; 3]) -> f64 {
    let (i, j) = PAIRS[a];
    if i == j { n[i] * m[i] } else { n[i] * m[j] + n[j] * m[i] }
}

fn divided(
    g: &impl SpectralScalar,
    a: f64,
    b: f64,
    da: (f64, f64, f64, f64),
    db: (f64, f64, f64, f64),
) -> f64 {
    let gap = a - b;
    let size = a.abs().max(b.abs()).max(f64::MIN_POSITIVE);
    if gap.abs() <= TAYLOR_GAP * size {
        let (_, _, g2, g4) = g.eval(0.5 * (a + b));
        g2 + g4 * gap * gap / 24.0
    } else {
        (da.1 - db.1) / gap
    }
}

pub fn trace_function<S: Scalar>(c: &Mat3<S>, g: &impl SpectralScalar) -> S {
    let inputs: [S; 6] = core::array::from_fn(|a| {
        let (i, j) = PAIRS[a];
        c[i][j]
    });
    let m: Mat3<f64> = core::array::from_fn(|i| {
        core::array::from_fn(|j| if i <= j { c[i][j].value() } else { c[j][i].value() })
    });
    let (values, vectors) = symmetric_eigen(&m);
    let d: [(f64, f64, f64, f64); 3] = core::array::from_fn(|k| g.eval(values[k]));
    let f = d[0].0 + d[1].0 + d[2].0;
    let mut grad = [0.0; 6];
    for (a, slot) in grad.iter_mut().enumerate() {
        *slot = (0..3).map(|k| d[k].1 * project(a, &vectors[k], &vectors[k])).sum();
    }
    let mut dd = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            dd[i][j] = divided(g, values[i], values[j], d[i], d[j]);
        }
    }
    let mut proj = [[[0.0; 3]; 3]; 6];
    for (a, p) in proj.iter_mut().enumerate() {
        for i in 0..3 {
            for j in 0..3 {
                p[i][j] = project(a, &vectors[i], &vectors[j]);
            }
        }
    }
    let mut hess = [0.0; 36];
    for a in 0..6 {
        for b in a..6 {
            let mut s = 0.0;
            for i in 0..3 {
                for j in 0..3 {
                    s += dd[i][j] * proj[a][i][j] * proj[b][i][j];
                }
            }
            hess[a * 6 + b] = s;
            hess[b * 6 + a] = s;
        }
    }
    S::lift(f, &inputs, &grad, &hess)
}

