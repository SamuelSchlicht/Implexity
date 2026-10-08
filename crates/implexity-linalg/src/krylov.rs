// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use crate::dense::{DenseMatrix, lstsq, qr_col_piv, svd_full};
use crate::error::LinalgError;
use crate::operator::{LinearOperator, axpy, dot, nrm2, scal};

#[derive(Clone, Debug, PartialEq)]
pub struct KrylovResult {
    pub x: Vec<f64>,
    pub info: usize,
    pub iterations: usize,
    pub residuals: Vec<f64>,
    pub matvecs: usize,
    pub psolves: usize,
}

struct Counted<'a, A: ?Sized, M: ?Sized> {
    a: &'a A,
    m: Option<&'a M>,
    matvecs: usize,
    psolves: usize,
}

impl<A: LinearOperator + ?Sized, M: LinearOperator + ?Sized> Counted<'_, A, M> {
    fn matvec(&mut self, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        self.matvecs += 1;
        let y = self.a.matvec(x)?;
        if y.len() != x.len() {
            return Err(LinalgError::Operator("operator returned a vector of the wrong length".into()));
        }
        Ok(y)
    }

    fn psolve(&mut self, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        match self.m {
            None => Ok(x.to_vec()),
            Some(m) => {
                self.psolves += 1;
                let y = m.matvec(x)?;
                if y.len() != x.len() {
                    return Err(LinalgError::Operator(
                        "preconditioner returned a vector of the wrong length".into(),
                    ));
                }
                Ok(y)
            }
        }
    }
}

fn check_system<A: LinearOperator + ?Sized, M: LinearOperator + ?Sized>(
    a: &A,
    m: Option<&M>,
    b: &[f64],
    x0: Option<&[f64]>,
    rtol: f64,
    atol: f64,
) -> Result<Vec<f64>, LinalgError> {
    let n = a.n();
    if b.len() != n {
        return Err(LinalgError::Shape(format!(
            "right-hand side of length {} for an operator of order {n}",
            b.len()
        )));
    }
    if let Some(m) = m
        && m.n() != n
    {
        return Err(LinalgError::Shape("preconditioner order differs from the operator".into()));
    }
    if !b.iter().all(|v| v.is_finite()) {
        return Err(LinalgError::NonFinite("RHS must contain only finite numbers".into()));
    }
    if atol < 0.0 || !atol.is_finite() || rtol < 0.0 || !rtol.is_finite() {
        return Err(LinalgError::Invalid(format!(
            "Krylov tolerances must be finite and non-negative: atol={atol}, rtol={rtol}"
        )));
    }
    match x0 {
        None => Ok(vec![0.0; n]),
        Some(x) if x.len() == n && x.iter().all(|v| v.is_finite()) => Ok(x.to_vec()),
        Some(x) if x.len() == n => Err(LinalgError::NonFinite("initial guess must contain only finite numbers".into())),
        Some(x) => Err(LinalgError::Shape(format!("initial guess of length {} for order {n}", x.len()))),
    }
}

fn lartg(f: f64, g: f64) -> (f64, f64, f64) {
    let safmin = f64::MIN_POSITIVE;
    let safmax = 1.0 / safmin;
    let rtmin = safmin.sqrt();
    let rtmax = (safmax / 2.0).sqrt();
    let f1 = f.abs();
    let g1 = g.abs();
    if g == 0.0 {
        (1.0, 0.0, f)
    } else if f == 0.0 {
        (0.0, g.signum(), g1)
    } else if f1 > rtmin && f1 < rtmax && g1 > rtmin && g1 < rtmax {
        let d = (f * f + g * g).sqrt();
        let c = f1 / d;
        let r = d.copysign(f);
        (c, g / r, r)
    } else {
        let u = safmax.min(safmin.max(f1).max(g1));
        let fs = f / u;
        let gs = g / u;
        let d = (fs * fs + gs * gs).sqrt();
        let c = fs.abs() / d;
        let r = d.copysign(f);
        let s = gs / r;
        (c, s, r * u)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GmresCallbackType {
    #[default]
    None,
    PrNorm,
    X,
    Legacy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GmresOptions {
    pub rtol: f64,
    pub atol: f64,
    pub restart: Option<usize>,
    pub maxiter: Option<usize>,
    pub callback_type: GmresCallbackType,
}

impl Default for GmresOptions {
    fn default() -> Self {
        Self { rtol: 1e-5, atol: 0.0, restart: None, maxiter: None, callback_type: GmresCallbackType::None }
    }
}




#[allow(clippy::too_many_lines)]
pub fn gmres<A, M>(
    a: &A,
    b: &[f64],
    x0: Option<&[f64]>,
    m: Option<&M>,
    opts: &GmresOptions,
) -> Result<KrylovResult, LinalgError>
where
    A: LinearOperator + ?Sized,
    M: LinearOperator + ?Sized,
{
    let mut x = check_system(a, m, b, x0, opts.rtol, opts.atol)?;
    if opts.maxiter == Some(0) {
        return Err(LinalgError::Invalid("Krylov iteration limit must be positive".into()));
    }
    let n = b.len();
    if opts.restart == Some(0) {
        return Err(LinalgError::Invalid("GMRES restart must be positive".into()));
    }
    let mut ops = Counted { a, m, matvecs: 0, psolves: 0 };
    let bnrm2 = nrm2(b);
    let atol = opts.atol.max(opts.rtol * bnrm2);
    if bnrm2 == 0.0 {
        return Ok(KrylovResult {
            x: b.to_vec(),
            info: 0,
            iterations: 0,
            residuals: Vec::new(),
            matvecs: 0,
            psolves: 0,
        });
    }
    let eps = f64::EPSILON;
    let maxiter = opts.maxiter.unwrap_or(n * 10);
    let restart = opts.restart.unwrap_or(20).min(n);
    let legacy = opts.callback_type == GmresCallbackType::Legacy;
    let mb_nrm2 = nrm2(&ops.psolve(b)?);
    let mut ptol_max_factor = 1.0f64;
    let mut ptol = mb_nrm2 * ptol_max_factor.min(atol / bnrm2);
    let mut presid = 0.0f64;
    let mut v = vec![vec![0.0; n]; restart + 1];
    let mut h = vec![vec![0.0; restart + 1]; restart];
    let mut givens = vec![(0.0f64, 0.0f64); restart];
    let mut inner_iter = 0usize;
    let mut residuals = Vec::new();
    let mut r = Vec::new();
    let mut rnorm = f64::INFINITY;
    for iteration in 0..maxiter {
        if iteration == 0 {
            r = if x.iter().any(|&e| e != 0.0) {
                let ax = ops.matvec(&x)?;
                b.iter().zip(&ax).map(|(p, q)| p - q).collect()
            } else {
                b.to_vec()
            };
            if nrm2(&r) < atol {
                return Ok(KrylovResult {
                    x,
                    info: 0,
                    iterations: 0,
                    residuals,
                    matvecs: ops.matvecs,
                    psolves: ops.psolves,
                });
            }
        }
        v[0] = ops.psolve(&r)?;
        let tmp = nrm2(&v[0]);
        scal(1.0 / tmp, &mut v[0]);
        let mut s = vec![0.0; restart + 1];
        s[0] = tmp;
        let mut breakdown = false;
        let mut col = 0;
        for c in 0..restart {
            col = c;
            let av = ops.matvec(&v[c])?;
            let mut w = ops.psolve(&av)?;
            let h0 = nrm2(&w);
            for k in 0..=c {
                let t = dot(&v[k], &w);
                h[c][k] = t;
                axpy(-t, &v[k], &mut w);
            }
            let h1 = nrm2(&w);
            h[c][c + 1] = h1;
            v[c + 1].copy_from_slice(&w);
            if h1 <= eps * h0 {
                h[c][c + 1] = 0.0;
                breakdown = true;
            } else {
                scal(1.0 / h1, &mut v[c + 1]);
            }
            for k in 0..c {
                let (cs, sn) = givens[k];
                let (n0, n1) = (h[c][k], h[c][k + 1]);
                h[c][k] = cs * n0 + sn * n1;
                h[c][k + 1] = -sn * n0 + cs * n1;
            }
            let (cs, sn, mag) = lartg(h[c][c], h[c][c + 1]);
            givens[c] = (cs, sn);
            h[c][c] = mag;
            h[c][c + 1] = 0.0;
            let t = -sn * s[c];
            s[c] *= cs;
            s[c + 1] = t;
            presid = t.abs();
            inner_iter += 1;
            residuals.push(presid / bnrm2);
            if legacy && inner_iter == maxiter {
                break;
            }
            if presid <= ptol || breakdown {
                break;
            }
        }
        if h[col][col] == 0.0 {
            s[col] = 0.0;
        }
        let mut y = s[..=col].to_vec();
        for k in (1..=col).rev() {
            if y[k] != 0.0 {
                y[k] /= h[k][k];
                let t = y[k];
                for i in 0..k {
                    y[i] -= t * h[k][i];
                }
            }
        }
        if y[0] != 0.0 {
            y[0] /= h[0][0];
        }
        for (k, &yk) in y.iter().enumerate() {
            axpy(yk, &v[k], &mut x);
        }
        let ax = ops.matvec(&x)?;
        r = b.iter().zip(&ax).map(|(p, q)| p - q).collect();
        rnorm = nrm2(&r);
        if legacy && inner_iter == maxiter {
            let info = if rnorm <= atol { 0 } else { maxiter };
            return Ok(KrylovResult {
                x,
                info,
                iterations: inner_iter,
                residuals,
                matvecs: ops.matvecs,
                psolves: ops.psolves,
            });
        }
        if rnorm <= atol || breakdown {
            break;
        } else if presid <= ptol {
            ptol_max_factor = eps.max(0.25 * ptol_max_factor);
        } else {
            ptol_max_factor = (1.5 * ptol_max_factor).min(1.0);
        }
        ptol = presid * ptol_max_factor.min(atol / rnorm);
    }
    let info = if rnorm <= atol { 0 } else { maxiter };
    Ok(KrylovResult {
        x,
        info,
        iterations: inner_iter,
        residuals,
        matvecs: ops.matvecs,
        psolves: ops.psolves,
    })
}

struct Fgmres {
    h: Vec<Vec<f64>>,
    b: Vec<Vec<f64>>,
    vs: Vec<Vec<f64>>,
    zs: Vec<Vec<f64>>,
    y: Vec<f64>,
    res: f64,
    r_square: Vec<Vec<f64>>,
}

impl Fgmres {
    fn hy(&self, y: &[f64]) -> Vec<f64> {
        let rows = self.h.len() + 1;
        let mut out = vec![0.0; rows];
        for (col, yc) in self.h.iter().zip(y) {
            for (o, hv) in out.iter_mut().zip(col) {
                *o += hv * yc;
            }
        }
        out
    }
}

pub type OuterVector = (Vec<f64>, Option<Vec<f64>>);

type CuPair = (Vec<f64>, Vec<f64>);

enum Augment<'v> {
    None,
    Outer { vectors: &'v [OuterVector], prepend: bool },
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn fgmres<A: LinearOperator + ?Sized, M: LinearOperator + ?Sized>(
    ops: &mut Counted<'_, A, M>,
    v0: Vec<f64>,
    m: usize,
    atol: f64,
    left: bool,
    cs: &[&[f64]],
    augment: &Augment<'_>,
) -> Result<Fgmres, LinalgError> {
    let (outer, prepend): (&[OuterVector], bool) = match augment {
        Augment::None => (&[], false),
        Augment::Outer { vectors, prepend } => (vectors, *prepend),
    };
    let m = m + outer.len();
    let eps = f64::EPSILON;
    let mut vs = vec![v0];
    let mut zs: Vec<Vec<f64>> = Vec::new();
    let mut bproj = vec![vec![0.0; m]; cs.len()];
    let mut h_cols: Vec<Vec<f64>> = Vec::new();
    let mut r_cols: Vec<Vec<f64>> = Vec::new();
    let mut rots: Vec<(f64, f64)> = Vec::new();
    let mut g = vec![1.0];
    let mut res = f64::NAN;
    let mut breakdown = false;
    let mut j_last = 0;
    for j in 0..m {
        j_last = j;
        let (z, given_w): (Vec<f64>, Option<Vec<f64>>) = if prepend && j < outer.len() {
            (outer[j].0.clone(), outer[j].1.clone())
        } else if prepend && j == outer.len() {
            (if left { vs[0].clone() } else { ops.psolve(&vs[0])? }, None)
        } else if !prepend && j >= m - outer.len() {
            let o = &outer[j - (m - outer.len())];
            (o.0.clone(), o.1.clone())
        } else {
            let last = vs.last().ok_or_else(|| LinalgError::Shape("empty Arnoldi basis".into()))?;
            (if left { last.clone() } else { ops.psolve(last)? }, None)
        };
        let mut w = if let Some(w) = given_w {
            w
        } else {
            let az = ops.matvec(&z)?;
            if left { ops.psolve(&az)? } else { az }
        };
        let w_norm = nrm2(&w);
        for (i, c) in cs.iter().enumerate() {
            let alpha = dot(c, &w);
            bproj[i][j] = alpha;
            axpy(-alpha, c, &mut w);
        }
        let mut hcur = vec![0.0; j + 2];
        for (i, v) in vs.iter().enumerate() {
            let alpha = dot(v, &w);
            hcur[i] = alpha;
            axpy(-alpha, v, &mut w);
        }
        hcur[j + 1] = nrm2(&w);
        let alpha = 1.0 / hcur[j + 1];
        if alpha.is_finite() {
            scal(alpha, &mut w);
        }

        if hcur[j + 1].is_nan() || hcur[j + 1] <= eps * w_norm {
            breakdown = true;
        }
        vs.push(w);
        zs.push(z);

        let mut rc = hcur.clone();
        for (k, &(c, s)) in rots.iter().enumerate() {
            let (a0, a1) = (rc[k], rc[k + 1]);
            rc[k] = c * a0 + s * a1;
            rc[k + 1] = -s * a0 + c * a1;
        }
        let (c, s, r) = lartg(rc[j], rc[j + 1]);
        rc[j] = r;
        rc[j + 1] = 0.0;
        rots.push((c, s));
        g.push(0.0);
        let (g0, g1) = (g[j], g[j + 1]);
        g[j] = c * g0 + s * g1;
        g[j + 1] = -s * g0 + c * g1;
        h_cols.push(hcur);
        r_cols.push(rc);
        res = g[j + 1].abs();
        if res < atol || breakdown {
            break;
        }
    }
    let j = j_last;
    if !r_cols[j][j].is_finite() {
        return Err(LinalgError::NonFinite("nans encountered in the Arnoldi process".into()));
    }
    let k = j + 1;
    let mut rmat = DenseMatrix::zeros(k, k);
    for (col, rc) in r_cols.iter().enumerate() {
        for (row, &v) in rc.iter().enumerate().take(col + 1) {
            rmat.data[row * k + col] = v;
        }
    }
    let y = lstsq(&rmat, &g[..k], None)?;
    for row in &mut bproj {
        row.truncate(k);
    }
    Ok(Fgmres { h: h_cols, b: bproj, vs, zs, y, res, r_square: r_cols })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LgmresOptions {
    pub rtol: f64,
    pub atol: f64,
    pub maxiter: usize,
    pub inner_m: usize,
    pub outer_k: usize,
    pub store_outer_av: bool,
    pub prepend_outer_v: bool,
}

impl Default for LgmresOptions {
    fn default() -> Self {
        Self {
            rtol: 1e-5,
            atol: 0.0,
            maxiter: 1000,
            inner_m: 30,
            outer_k: 3,
            store_outer_av: true,
            prepend_outer_v: false,
        }
    }
}




pub fn lgmres<A, M>(
    a: &A,
    b: &[f64],
    x0: Option<&[f64]>,
    m: Option<&M>,
    opts: &LgmresOptions,
    outer_v: &mut Vec<OuterVector>,
) -> Result<KrylovResult, LinalgError>
where
    A: LinearOperator + ?Sized,
    M: LinearOperator + ?Sized,
{
    let mut x = check_system(a, m, b, x0, opts.rtol, opts.atol)?;
    if opts.maxiter == 0 || opts.inner_m == 0 {
        return Err(LinalgError::Invalid("LGMRES iteration limits must be positive".into()));
    }
    let mut ops = Counted { a, m, matvecs: 0, psolves: 0 };
    let b_norm = nrm2(b);
    let atol = opts.atol.max(opts.rtol * b_norm);
    if b_norm == 0.0 {
        return Ok(KrylovResult {
            x: b.to_vec(),
            info: 0,
            iterations: 0,
            residuals: Vec::new(),
            matvecs: 0,
            psolves: 0,
        });
    }
    let mut ptol_max_factor = 1.0f64;
    let mut residuals = Vec::new();
    for k_outer in 0..opts.maxiter {
        let ax = ops.matvec(&x)?;
        let r_outer: Vec<f64> = ax.iter().zip(b).map(|(p, q)| p - q).collect();
        let r_norm = nrm2(&r_outer);
        residuals.push(r_norm);
        if r_norm <= atol.max(opts.rtol * b_norm) {
            return Ok(KrylovResult {
                x,
                info: 0,
                iterations: k_outer + 1,
                residuals,
                matvecs: ops.matvecs,
                psolves: ops.psolves,
            });
        }
        let mut v0: Vec<f64> = ops.psolve(&r_outer)?.into_iter().map(|e| -e).collect();
        let inner_res_0 = nrm2(&v0);
        if inner_res_0 == 0.0 {
            return Err(LinalgError::Operator(format!(
                "Preconditioner returned a zero vector; |v| ~ {r_norm:.1e}, |M v| = 0"
            )));
        }
        scal(1.0 / inner_res_0, &mut v0);
        let ptol = ptol_max_factor.min(atol.max(opts.rtol * b_norm) / r_norm);
        let aug = Augment::Outer { vectors: outer_v.as_slice(), prepend: opts.prepend_outer_v };
        let fg = match fgmres(&mut ops, v0, opts.inner_m, ptol, true, &[], &aug) {
            Ok(fg) if fg.y.iter().all(|v| v.is_finite()) => fg,
            Ok(_) | Err(LinalgError::NonFinite(_)) => {
                return Ok(KrylovResult {
                    x,
                    info: k_outer + 1,
                    iterations: k_outer + 1,
                    residuals,
                    matvecs: ops.matvecs,
                    psolves: ops.psolves,
                });
            }
            Err(e) => return Err(e),
        };
        let y: Vec<f64> = fg.y.iter().map(|v| v * inner_res_0).collect();
        if fg.res > ptol {
            ptol_max_factor = (1.5 * ptol_max_factor).min(1.0);
        } else {
            ptol_max_factor = (0.25 * ptol_max_factor).max(1e-16);
        }
        let mut dx: Vec<f64> = fg.zs[0].iter().map(|v| v * y[0]).collect();
        for (w, &yc) in fg.zs.iter().zip(&y).skip(1) {
            axpy(yc, w, &mut dx);
        }
        let nx = nrm2(&dx);
        if nx > 0.0 {
            if opts.store_outer_av {
                let q = fg.hy(&y);
                let mut axv: Vec<f64> = fg.vs[0].iter().map(|v| v * q[0]).collect();
                for (v, &qc) in fg.vs.iter().zip(&q).skip(1) {
                    axpy(qc, v, &mut axv);
                }
                outer_v
                    .push((dx.iter().map(|v| v / nx).collect(), Some(axv.iter().map(|v| v / nx).collect())));
            } else {
                outer_v.push((dx.iter().map(|v| v / nx).collect(), None));
            }
        }
        while outer_v.len() > opts.outer_k {
            outer_v.remove(0);
        }
        for (xi, d) in x.iter_mut().zip(&dx) {
            *xi += d;
        }
    }
    Ok(KrylovResult {
        x,
        info: opts.maxiter,
        iterations: opts.maxiter,
        residuals,
        matvecs: ops.matvecs,
        psolves: ops.psolves,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Truncate {
    #[default]
    Oldest,
    Smallest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecyclePair {
    pub c: Option<Vec<f64>>,
    pub u: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GcrotOptions {
    pub rtol: f64,
    pub atol: f64,
    pub maxiter: usize,
    pub m: usize,
    pub k: Option<usize>,
    pub discard_c: bool,
    pub truncate: Truncate,
}

impl Default for GcrotOptions {
    fn default() -> Self {
        Self {
            rtol: 1e-5,
            atol: 0.0,
            maxiter: 1000,
            m: 20,
            k: None,
            discard_c: false,
            truncate: Truncate::Oldest,
        }
    }
}




#[allow(clippy::too_many_lines)]
pub fn gcrotmk<A, M>(
    a: &A,
    b: &[f64],
    x0: Option<&[f64]>,
    m_op: Option<&M>,
    opts: &GcrotOptions,
    cu: &mut Vec<RecyclePair>,
) -> Result<KrylovResult, LinalgError>
where
    A: LinearOperator + ?Sized,
    M: LinearOperator + ?Sized,
{
    let mut x = check_system(a, m_op, b, x0, opts.rtol, opts.atol)?;
    if opts.maxiter == 0 || opts.m == 0 {
        return Err(LinalgError::Invalid("GCROT iteration limits must be positive".into()));
    }
    let n = b.len();
    let mut ops = Counted { a, m: m_op, matvecs: 0, psolves: 0 };
    let k = opts.k.unwrap_or(opts.m);
    let mut r: Vec<f64> = if x0.is_none() {
        b.to_vec()
    } else {
        let ax = ops.matvec(&x)?;
        b.iter().zip(&ax).map(|(p, q)| p - q).collect()
    };
    let b_norm = nrm2(b);
    let atol = opts.atol.max(opts.rtol * b_norm);
    if b_norm == 0.0 {
        return Ok(KrylovResult {
            x: b.to_vec(),
            info: 0,
            iterations: 0,
            residuals: Vec::new(),
            matvecs: 0,
            psolves: 0,
        });
    }
    if opts.discard_c {
        for p in cu.iter_mut() {
            p.c = None;
        }
    }

    let mut pairs: Vec<CuPair> = Vec::new();
    if !cu.is_empty() {

        cu.sort_by_key(|p| p.c.is_some());
        let taken: Vec<RecyclePair> = std::mem::take(cu);
        let mut cmat = DenseMatrix::zeros(n, taken.len());
        let mut us = Vec::with_capacity(taken.len());
        for (j, p) in taken.into_iter().enumerate() {
            let c = match p.c {
                Some(c) => c,
                None => ops.matvec(&p.u)?,
            };
            for (i, &ci) in c.iter().enumerate() {
                cmat.data[i * cmat.ncols + j] = ci;
            }
            us.push(p.u);
        }
        let (q, rr, perm) = qr_col_piv(&cmat)?;
        let ncs = q.ncols;

        let mut new_us: Vec<Vec<f64>> = Vec::new();
        for j in 0..ncs {
            let mut u = us[perm[j]].clone();
            for (i, prev) in new_us.iter().enumerate().take(j) {
                axpy(-rr.get(i, j), prev, &mut u);
            }
            if rr.get(j, j).abs() < 1e-12 * rr.get(0, 0).abs() {
                break;
            }
            scal(1.0 / rr.get(j, j), &mut u);
            new_us.push(u);
        }
        let cs: Vec<Vec<f64>> = (0..ncs).map(|j| (0..n).map(|i| q.get(i, j)).collect()).collect();
        pairs = cs.into_iter().zip(new_us).collect();
        pairs.reverse();
    }
    if !pairs.is_empty() {
        for (c, u) in &pairs {
            let yc = dot(c, &r);
            axpy(yc, u, &mut x);
            axpy(-yc, c, &mut r);
        }
    }
    let mut residuals = Vec::new();
    let mut j_outer_final: Option<usize> = None;
    let mut converged = false;
    for j_outer in 0..opts.maxiter {
        let mut beta = nrm2(&r);
        let beta_tol = atol.max(opts.rtol * b_norm);
        residuals.push(beta);
        if beta <= beta_tol && (j_outer > 0 || !pairs.is_empty()) {
            let ax = ops.matvec(&x)?;
            r = b.iter().zip(&ax).map(|(p, q)| p - q).collect();
            beta = nrm2(&r);
        }
        if beta <= beta_tol {
            converged = true;
            break;
        }
        let ml = opts.m + k.saturating_sub(pairs.len());
        let cs: Vec<&[f64]> = pairs.iter().map(|(c, _)| c.as_slice()).collect();
        let v0: Vec<f64> = r.iter().map(|v| v / beta).collect();
        let fg =
            match fgmres(&mut ops, v0, ml, atol.max(opts.rtol * b_norm) / beta, false, &cs, &Augment::None) {
                Ok(fg) => fg,
                Err(LinalgError::NonFinite(_)) => {
                    j_outer_final = Some(j_outer);
                    break;
                }
                Err(e) => return Err(e),
            };
        let y: Vec<f64> = fg.y.iter().map(|v| v * beta).collect();
        let mut ux: Vec<f64> = fg.zs[0].iter().map(|v| v * y[0]).collect();
        for (z, &yc) in fg.zs.iter().zip(&y).skip(1) {
            axpy(yc, z, &mut ux);
        }
        let by: Vec<f64> = fg.b.iter().map(|row| row.iter().zip(&y).map(|(p, q)| p * q).sum()).collect();
        for ((_, u), &byc) in pairs.iter().zip(&by) {
            axpy(-byc, u, &mut ux);
        }
        let hy = fg.hy(&y);
        let mut cx: Vec<f64> = fg.vs[0].iter().map(|v| v * hy[0]).collect();
        for (v, &hyc) in fg.vs.iter().zip(&hy).skip(1) {
            axpy(hyc, v, &mut cx);
        }
        let alpha = 1.0 / nrm2(&cx);
        if !alpha.is_finite() {
            continue;
        }
        scal(alpha, &mut cx);
        scal(alpha, &mut ux);
        let gamma = dot(&cx, &r);
        axpy(-gamma, &cx, &mut r);
        axpy(gamma, &ux, &mut x);
        match opts.truncate {
            Truncate::Oldest => {
                while pairs.len() >= k && !pairs.is_empty() {
                    pairs.remove(0);
                }
            }
            Truncate::Smallest => {
                if pairs.len() >= k && !pairs.is_empty() {

                    pairs = if let Ok(kept) = truncate_smallest(&pairs, &fg, k) {
                        kept
                    } else {
                        let mut kept = pairs;
                        while kept.len() >= k && !kept.is_empty() {
                            kept.remove(0);
                        }
                        kept
                    };
                }
            }
        }
        pairs.push((cx, ux));
    }
    let iterations = residuals.len();
    let info = if converged {
        0
    } else {
        match j_outer_final {
            Some(j) => j + 1,
            None => opts.maxiter,
        }
    };

    *cu = pairs.into_iter().map(|(c, u)| RecyclePair { c: Some(c), u }).collect();
    cu.push(RecyclePair { c: None, u: x.clone() });
    if opts.discard_c {
        for p in cu.iter_mut() {
            p.c = None;
        }
    }
    Ok(KrylovResult { x, info, iterations, residuals, matvecs: ops.matvecs, psolves: ops.psolves })
}



fn truncate_smallest(pairs: &[CuPair], fg: &Fgmres, k: usize) -> Result<Vec<CuPair>, LinalgError> {
    let ncu = pairs.len();
    let jj = fg.r_square.len();
    if (0..jj).any(|c| {
        let d = fg.r_square[c].get(c).copied().unwrap_or(0.0);
        d == 0.0 || !d.is_finite()
    }) {
        return Err(LinalgError::NonFinite("singular or non-finite FGMRES triangle in truncation".into()));
    }
    let mut d = DenseMatrix::zeros(ncu, jj);
    for (row_i, brow) in fg.b.iter().enumerate() {
        let mut drow = vec![0.0; jj];
        for col in 0..jj {
            let mut acc = brow[col];
            for (i, dv) in drow.iter().enumerate().take(col) {
                acc -= dv * fg.r_square[col][i];
            }
            drow[col] = acc / fg.r_square[col][col];
        }
        d.data[row_i * jj..(row_i + 1) * jj].copy_from_slice(&drow);
    }
    if d.data.iter().any(|v| !v.is_finite()) {
        return Err(LinalgError::NonFinite("non-finite B R^-1 in truncation".into()));
    }
    let w = svd_full(&d)?.u;
    let n = pairs[0].0.len();
    let mut new_cu: Vec<CuPair> = Vec::new();
    for j in 0..k.saturating_sub(1).min(w.ncols) {
        let mut c: Vec<f64> = pairs[0].0.iter().map(|v| v * w.get(0, j)).collect();
        let mut u: Vec<f64> = pairs[0].1.iter().map(|v| v * w.get(0, j)).collect();
        for (p, (cp, up)) in pairs.iter().enumerate().skip(1) {
            axpy(w.get(p, j), cp, &mut c);
            axpy(w.get(p, j), up, &mut u);
        }
        for (cp, up) in &new_cu {
            let alpha = dot(cp, &c);
            axpy(-alpha, cp, &mut c);
            axpy(-alpha, up, &mut u);
        }
        let alpha = nrm2(&c);
        if alpha == 0.0 || !alpha.is_finite() {
            continue;
        }
        scal(1.0 / alpha, &mut c);
        scal(1.0 / alpha, &mut u);
        debug_assert_eq!(c.len(), n);
        new_cu.push((c, u));
    }
    Ok(new_cu)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CgOptions {
    pub rtol: f64,
    pub atol: f64,
    pub maxiter: Option<usize>,
}

impl Default for CgOptions {
    fn default() -> Self {
        Self { rtol: 1e-5, atol: 0.0, maxiter: None }
    }
}




pub fn cg<A, M>(
    a: &A,
    b: &[f64],
    x0: Option<&[f64]>,
    m: Option<&M>,
    opts: &CgOptions,
) -> Result<KrylovResult, LinalgError>
where
    A: LinearOperator + ?Sized,
    M: LinearOperator + ?Sized,
{
    let mut x = check_system(a, m, b, x0, opts.rtol, opts.atol)?;
    if opts.maxiter == Some(0) {
        return Err(LinalgError::Invalid("Krylov iteration limit must be positive".into()));
    }
    let n = b.len();
    let mut ops = Counted { a, m, matvecs: 0, psolves: 0 };
    let bnrm2 = nrm2(b);
    let atol = opts.atol.max(opts.rtol * bnrm2);
    if bnrm2 == 0.0 {
        return Ok(KrylovResult {
            x: b.to_vec(),
            info: 0,
            iterations: 0,
            residuals: Vec::new(),
            matvecs: 0,
            psolves: 0,
        });
    }
    let maxiter = opts.maxiter.unwrap_or(n * 10);
    let mut r: Vec<f64> = if x.iter().any(|&v| v != 0.0) {
        let ax = ops.matvec(&x)?;
        b.iter().zip(&ax).map(|(p, q)| p - q).collect()
    } else {
        b.to_vec()
    };
    let mut rho_prev = 0.0;
    let mut p: Vec<f64> = Vec::new();
    let mut residuals = Vec::new();
    for iteration in 0..maxiter {
        if nrm2(&r) < atol {
            return Ok(KrylovResult {
                x,
                info: 0,
                iterations: iteration,
                residuals,
                matvecs: ops.matvecs,
                psolves: ops.psolves,
            });
        }
        let z = ops.psolve(&r)?;
        let rho_cur = dot(&r, &z);
        if iteration > 0 {
            let beta = rho_cur / rho_prev;
            scal(beta, &mut p);
            for (pi, zi) in p.iter_mut().zip(&z) {
                *pi += zi;
            }
        } else {
            p = z;
        }
        let q = ops.matvec(&p)?;
        let alpha = rho_cur / dot(&p, &q);
        axpy(alpha, &p, &mut x);
        axpy(-alpha, &q, &mut r);
        rho_prev = rho_cur;
        residuals.push(nrm2(&r));
    }
    Ok(KrylovResult {
        x,
        info: maxiter,
        iterations: maxiter,
        residuals,
        matvecs: ops.matvecs,
        psolves: ops.psolves,
    })
}

