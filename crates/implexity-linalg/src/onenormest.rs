// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use crate::error::LinalgError;
use crate::operator::LinearOperator;

#[derive(Clone, Debug, PartialEq)]
pub struct OneNormEstimate {
    pub estimate: f64,
    pub v: Vec<f64>,
    pub w: Vec<f64>,
    pub products: usize,
    pub resamples: usize,
}

struct SignSource(u64);

impl SignSource {
    fn next_sign(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        if z & 1 == 0 { -1.0 } else { 1.0 }
    }
}

fn parallel(v: &[f64], w: &[f64]) -> bool {
    #[allow(clippy::float_cmp)]                                                     
    let r = v.iter().zip(w).map(|(a, b)| a * b).sum::<f64>() == v.len() as f64;
    r
}

fn col(x: &[Vec<f64>], j: usize) -> &[f64] {
    &x[j]
}

fn needs_resampling(i: usize, x: &[Vec<f64>], y: Option<&[Vec<f64>]>) -> bool {
    let v = col(x, i);
    if (0..i).any(|j| parallel(v, col(x, j))) {
        return true;
    }
    y.is_some_and(|y| y.iter().any(|w| parallel(v, w)))
}

fn top_descending(h: &[f64], k: usize) -> Vec<usize> {
    let order = |p: &usize, q: &usize| h[*q].total_cmp(&h[*p]).then(q.cmp(p));
    let mut ind: Vec<usize> = (0..h.len()).collect();
    if k < ind.len() {
        ind.select_nth_unstable_by(k, order);
        ind.truncate(k);
    }
    ind.sort_unstable_by(order);
    ind
}

fn matmat(op: &dyn LinearOperator, x: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, LinalgError> {
    let y = op.apply_block(x)?;
    if y.len() != x.len() || y.iter().any(|c| c.len() != op.n()) {
        return Err(LinalgError::Shape("operator block action returned another shape".into()));
    }
    Ok(y)
}



pub fn onenormest(a: &dyn LinearOperator, at: &dyn LinearOperator) -> Result<OneNormEstimate, LinalgError> {
    onenormest_with(a, at, 2, 5)
}



#[allow(clippy::too_many_lines)]
pub fn onenormest_with(
    a: &dyn LinearOperator,
    at: &dyn LinearOperator,
    t: usize,
    itmax: usize,
) -> Result<OneNormEstimate, LinalgError> {
    let n = a.n();
    if at.n() != n {
        return Err(LinalgError::Shape("operator and transpose differ in order".into()));
    }
    if n == 0 {
        return Ok(OneNormEstimate {
            estimate: 0.0,
            v: Vec::new(),
            w: Vec::new(),
            products: 0,
            resamples: 0,
        });
    }
    if t >= n {

        let mut best = (0usize, -1.0f64, Vec::new());
        for j in 0..n {
            let mut e = vec![0.0; n];
            e[j] = 1.0;
            let c = a.matvec(&e)?;
            let s: f64 = c.iter().map(|v| v.abs()).sum();
            if s > best.1 {
                best = (j, s, c);
            }
        }
        let mut v = vec![0.0; n];
        v[best.0] = 1.0;
        return Ok(OneNormEstimate { estimate: best.1, v, w: best.2, products: n, resamples: 0 });
    }
    if itmax < 2 {
        return Err(LinalgError::Invalid("at least two iterations are required".into()));
    }
    if t < 1 {
        return Err(LinalgError::Invalid("at least one column is required".into()));
    }
    let mut rng = SignSource(0x5EED_0000_0001);
    let mut products = 0usize;
    let mut resamples = 0usize;
    let mut x: Vec<Vec<f64>> = vec![vec![1.0; n]; t];
    if t > 1 {
        for c in x.iter_mut().skip(1) {
            for e in c.iter_mut() {
                *e = rng.next_sign();
            }
        }
        for i in 0..t {
            while needs_resampling(i, &x, None) {
                for e in &mut x[i] {
                    *e = rng.next_sign();
                }
                resamples += 1;
            }
        }
    }
    for c in &mut x {
        for e in c.iter_mut() {
            *e /= n as f64;
        }
    }
    let mut ind_hist: Vec<usize> = Vec::new();
    let mut est_old = 0.0f64;
    let mut s: Vec<Vec<f64>> = vec![vec![0.0; n]; t];
    let mut k = 1usize;
    let mut ind: Vec<usize> = Vec::new();
    let mut ind_best = 0usize;
    let mut w: Vec<f64> = Vec::new();
    let mut est;
    loop {
        let y = matmat(a, &x)?;
        products += 1;
        let mags: Vec<f64> = y.iter().map(|c| c.iter().map(|v| v.abs()).sum()).collect();
        let mut best_j = 0;
        for (j, &m) in mags.iter().enumerate() {
            if m > mags[best_j] {
                best_j = j;
            }
        }
        est = mags[best_j];
        if est > est_old || k == 2 {
            if k >= 2 {
                ind_best = ind[best_j];
            }
            w.clone_from(&y[best_j]);
        }
        if k >= 2 && est <= est_old {
            est = est_old;
            break;
        }
        est_old = est;
        let s_old = s;
        if k > itmax {
            break;
        }
        s = y.iter().map(|c| c.iter().map(|&v| if v == 0.0 { 1.0 } else { v.signum() }).collect()).collect();
        if s.iter().all(|v| s_old.iter().any(|u| parallel(v, u))) {
            break;
        }
        if t > 1 {
            for i in 0..t {
                while needs_resampling(i, &s, Some(&s_old)) {
                    for e in &mut s[i] {
                        *e = rng.next_sign();
                    }
                    resamples += 1;
                }
            }
        }
        let z = matmat(at, &s)?;
        products += 1;
        let h: Vec<f64> =
            (0..n).map(|r| z.iter().map(|c| c[r].abs()).fold(f64::NEG_INFINITY, f64::max)).collect();
        let hmax = h.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        #[allow(clippy::float_cmp)]                                  
        let stop = k >= 2 && hmax == h[ind_best];
        if stop {
            break;
        }

        ind = top_descending(&h, t + ind_hist.len());
        if t > 1 {
            if ind.iter().take(t).all(|i| ind_hist.contains(i)) {
                break;
            }
            let (unseen, seen): (Vec<usize>, Vec<usize>) = ind.iter().partition(|i| !ind_hist.contains(i));
            ind = unseen.into_iter().chain(seen).collect();
        }
        for (j, c) in x.iter_mut().enumerate() {
            c.fill(0.0);
            c[ind[j]] = 1.0;
        }
        let new_ind: Vec<usize> = ind.iter().take(t).filter(|i| !ind_hist.contains(i)).copied().collect();
        ind_hist.extend(new_ind);
        k += 1;
    }
    let mut v = vec![0.0; n];
    v[ind_best] = 1.0;
    Ok(OneNormEstimate { estimate: est, v, w, products, resamples })
}

