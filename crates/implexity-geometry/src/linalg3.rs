// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub type Mat<const N: usize> = [[f64; N]; N];

#[must_use]
pub fn lu<const N: usize>(a: &Mat<N>) -> Option<(Mat<N>, [usize; N], f64)> {
    let mut m = *a;
    let mut perm: [usize; N] = std::array::from_fn(|i| i);
    let mut sign = 1.0;
    for k in 0..N {
        let mut p = k;
        for i in k + 1..N {
            if m[i][k].abs() > m[p][k].abs() {
                p = i;
            }
        }
        if m[p][k] == 0.0 {
            return None;
        }
        if p != k {
            m.swap(p, k);
            perm.swap(p, k);
            sign = -sign;
        }
        for i in k + 1..N {
            let l = m[i][k] / m[k][k];
            m[i][k] = l;
            for j in k + 1..N {
                m[i][j] -= l * m[k][j];
            }
        }
    }
    Some((m, perm, sign))
}

#[must_use]
pub fn det<const N: usize>(a: &Mat<N>) -> f64 {
    match lu(a) {
        None => 0.0,
        Some((m, _, sign)) => {
            let mut d = sign;
            for (k, row) in m.iter().enumerate() {
                d *= row[k];
            }
            d
        }
    }
}

#[must_use]
pub fn solve<const N: usize>(a: &Mat<N>, b: &[f64; N]) -> Option<[f64; N]> {
    let (m, perm, _) = lu(a)?;
    let mut y: [f64; N] = std::array::from_fn(|i| b[perm[i]]);
    for i in 0..N {
        for j in 0..i {
            y[i] -= m[i][j] * y[j];
        }
    }
    for i in (0..N).rev() {
        for j in i + 1..N {
            y[i] -= m[i][j] * y[j];
        }
        y[i] /= m[i][i];
    }
    Some(y)
}

#[must_use]
pub fn inv<const N: usize>(a: &Mat<N>) -> Option<Mat<N>> {
    let mut out = [[0.0; N]; N];
    for j in 0..N {
        let e: [f64; N] = std::array::from_fn(|i| if i == j { 1.0 } else { 0.0 });
        let x = solve(a, &e)?;
        for i in 0..N {
            out[i][j] = x[i];
        }
    }
    Some(out)
}

#[must_use]
pub fn sym_eigenvalues<const N: usize>(a: &Mat<N>) -> [f64; N] {
    let mut m = *a;
    for _sweep in 0..64 {
        let mut off = 0.0;
        for i in 0..N {
            for j in 0..N {
                if i != j {
                    off += m[i][j] * m[i][j];
                }
            }
        }
        if off <= f64::MIN_POSITIVE {
            break;
        }
        for p in 0..N {
            for q in p + 1..N {
                if m[p][q] == 0.0 {
                    continue;
                }
                let theta = (m[q][q] - m[p][p]) / (2.0 * m[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..N {
                    let (mkp, mkq) = (m[k][p], m[k][q]);
                    m[k][p] = c * mkp - s * mkq;
                    m[k][q] = s * mkp + c * mkq;
                }
                for k in 0..N {
                    let (mpk, mqk) = (m[p][k], m[q][k]);
                    m[p][k] = c * mpk - s * mqk;
                    m[q][k] = s * mpk + c * mqk;
                }
            }
        }
    }
    let mut ev: [f64; N] = std::array::from_fn(|i| m[i][i]);
    ev.sort_by(|a, b| b.total_cmp(a));
    ev
}

#[must_use]
pub fn singular_values<const N: usize>(a: &Mat<N>) -> [f64; N] {
    let mut ata = [[0.0; N]; N];
    for i in 0..N {
        for j in 0..N {
            ata[i][j] = (0..N).map(|k| a[k][i] * a[k][j]).sum();
        }
    }
    sym_eigenvalues(&ata).map(|v| v.max(0.0).sqrt())
}

#[must_use]
pub fn matvec<const N: usize>(a: &Mat<N>, v: &[f64; N]) -> [f64; N] {
    std::array::from_fn(|i| {
        let mut s = 0.0;
        for (j, x) in v.iter().enumerate() {
            s += a[i][j] * x;
        }
        s
    })
}


