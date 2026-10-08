// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseMatrix, eigvalsh};
use implexity_linalg::sparse::CsrMatrix;
use implexity_linalg::spectral::{LanczosOptions, smallest_eigenvalue};
use serde_json::{Value, json};

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct MortarResult {
    pub coupled_matrix: DenseMatrix,
    pub constraint_matrix: DenseMatrix,
}

fn admit_dense_blocks(k1: &DenseMatrix, k2: &DenseMatrix, t1: &DenseMatrix, t2: &DenseMatrix) -> CaeResult<()> {
    if k1.nrows == 0 || k2.nrows == 0 || t1.nrows == 0
        || k1.ncols != k1.nrows || k2.ncols != k2.nrows
        || t1.nrows != t2.nrows || t1.ncols != k1.nrows || t2.ncols != k2.nrows
        || [k1, k2, t1, t2].iter().any(|m| m.nrows.checked_mul(m.ncols) != Some(m.data.len()))
    { return Err(err("interface dimensions invalid")); }
    if [k1, k2, t1, t2].iter().flat_map(|m| &m.data).any(|v| !v.is_finite()) {
        return Err(err("interface matrices must be finite"));
    }
    Ok(())
}

fn block_diag(k1: &DenseMatrix, k2: &DenseMatrix) -> DenseMatrix {
    let n = k1.nrows + k2.nrows;
    let mut k = DenseMatrix::zeros(n, n);
    for i in 0..k1.nrows {
        for j in 0..k1.ncols {
            k.data[i * n + j] = k1.get(i, j);
        }
    }
    for i in 0..k2.nrows {
        for j in 0..k2.ncols {
            k.data[(k1.nrows + i) * n + k1.ncols + j] = k2.get(i, j);
        }
    }
    k
}

fn hstack_neg(b1: &DenseMatrix, b2: &DenseMatrix) -> DenseMatrix {
    let cols = b1.ncols + b2.ncols;
    let mut c = DenseMatrix::zeros(b1.nrows, cols);
    for i in 0..b1.nrows {
        for j in 0..b1.ncols {
            c.data[i * cols + j] = b1.get(i, j);
        }
        for j in 0..b2.ncols {
            c.data[i * cols + b1.ncols + j] = -b2.get(i, j);
        }
    }
    c
}



pub fn mortar_saddle_matrix(
    k1: &DenseMatrix,
    k2: &DenseMatrix,
    b1: &DenseMatrix,
    b2: &DenseMatrix,
) -> CaeResult<MortarResult> {
    admit_dense_blocks(k1, k2, b1, b2)?;
    let c = hstack_neg(b1, b2);
    let k = block_diag(k1, k2);
    let n = k.nrows + c.nrows;
    let mut a = DenseMatrix::zeros(n, n);
    for i in 0..k.nrows {
        for j in 0..k.ncols {
            a.data[i * n + j] = k.get(i, j);
        }
        for r in 0..c.nrows {
            a.data[i * n + k.ncols + r] = c.get(r, i);
        }
    }
    for r in 0..c.nrows {
        for j in 0..c.ncols {
            a.data[(k.nrows + r) * n + j] = c.get(r, j);
        }
    }
    Ok(MortarResult { coupled_matrix: a, constraint_matrix: c })
}



pub fn symmetric_nitsche_matrix(
    k1: &DenseMatrix,
    k2: &DenseMatrix,
    t1: &DenseMatrix,
    t2: &DenseMatrix,
    penalty: f64,
) -> CaeResult<DenseMatrix> {
    admit_dense_blocks(k1, k2, t1, t2)?;
    if !penalty.is_finite() || penalty <= 0.0 {
        return Err(err("penalty must be positive"));
    }
    let t = hstack_neg(t1, t2);
    let mut a = block_diag(k1, k2);
    let tt = t.transpose().matmul(&t).map_err(|e| err(e.to_string()))?;
    for (x, y) in a.data.iter_mut().zip(&tt.data) {
        *x += penalty * y;
    }
    Ok(a)
}



pub fn variational_diagnostics(a: &DenseMatrix, sym_tol: f64) -> CaeResult<Value> {
    if a.nrows != a.ncols {
        return Err(err("variational diagnostics require a square matrix"));
    }
    let at = a.transpose();
    let diff: f64 = a.data.iter().zip(&at.data).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt();
    let sym = diff / a.norm_fro().max(1.0);
    let mut s = a.clone();
    for (x, y) in s.data.iter_mut().zip(&at.data) {
        *x = 0.5 * (*x + y);
    }
    let eig = eigvalsh(&s).map_err(|e| err(e.to_string()))?;
    let min = eig.iter().copied().fold(f64::INFINITY, f64::min);
    Ok(
        json!({"symmetry_error": sym, "symmetric": sym <= sym_tol, "minimum_symmetric_eigenvalue": min, "coercive": min > -sym_tol}),
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct SparseInterfaceSystem {
    pub matrix: CsrMatrix,
    pub symmetry_error: f64,
    pub minimum_eigenvalue: Option<f64>,
}

fn triplets_of(m: &CsrMatrix, row0: usize, col0: usize, scale: f64, t: &mut Vec<(usize, usize, f64)>) {
    for i in 0..m.nrows() {
        let (idx, data) = m.row(i);
        for (j, v) in idx.iter().zip(data) {
            t.push((row0 + i, col0 + j, scale * v));
        }
    }
}

fn build(n: usize, m: usize, t: &[(usize, usize, f64)]) -> CaeResult<CsrMatrix> {
    let rows: Vec<usize> = t.iter().map(|e| e.0).collect();
    let cols: Vec<usize> = t.iter().map(|e| e.1).collect();
    let vals: Vec<f64> = t.iter().map(|e| e.2).collect();
    CsrMatrix::from_triplets(n, m, &rows, &cols, &vals).map_err(|e| err(e.to_string()))
}

fn symmetry(a: &CsrMatrix) -> CaeResult<f64> {
    let d = a.add_scaled(1.0, &a.transpose(), -1.0).map_err(|e| err(e.to_string()))?;
    Ok(d.norm_fro() / a.norm_fro().max(1e-30))
}



pub fn sparse_mortar(
    k1: &CsrMatrix,
    k2: &CsrMatrix,
    b1: &CsrMatrix,
    b2: &CsrMatrix,
) -> CaeResult<SparseInterfaceSystem> {
    let (n1, n2, m) = (k1.nrows(), k2.nrows(), b1.nrows());
    if b2.nrows() != m || b1.ncols() != n1 || b2.ncols() != n2 || k1.ncols() != n1 || k2.ncols() != n2 {
        return Err(err("mortar dimensions invalid"));
    }
    let n = n1 + n2 + m;
    let mut t = Vec::new();
    triplets_of(k1, 0, 0, 1.0, &mut t);
    triplets_of(&b1.transpose(), 0, n1 + n2, 1.0, &mut t);
    triplets_of(k2, n1, n1, 1.0, &mut t);
    triplets_of(&b2.transpose(), n1, n1 + n2, -1.0, &mut t);
    triplets_of(b1, n1 + n2, 0, 1.0, &mut t);
    triplets_of(b2, n1 + n2, n1, -1.0, &mut t);
    let a = build(n, n, &t)?;
    let sym = symmetry(&a)?;
    Ok(SparseInterfaceSystem { matrix: a, symmetry_error: sym, minimum_eigenvalue: None })
}



pub fn sparse_symmetric_nitsche(
    k1: &CsrMatrix,
    k2: &CsrMatrix,
    b1: &CsrMatrix,
    b2: &CsrMatrix,
    penalty: f64,
) -> CaeResult<SparseInterfaceSystem> {
    let (n1, n2) = (k1.nrows(), k2.nrows());
    if n1 == 0 || n2 == 0 || b1.nrows() == 0 || k1.ncols() != n1 || k2.ncols() != n2
        || b1.nrows() != b2.nrows() || b1.ncols() != n1 || b2.ncols() != n2 {
        return Err(err("trace dimensions invalid"));
    }
    if [k1, k2, b1, b2].iter().any(|m| (0..m.nrows()).any(|r| m.row(r).1.iter().any(|v| !v.is_finite()))) {
        return Err(err("interface matrices must be finite"));
    }
    if !penalty.is_finite() || penalty <= 0.0 {
        return Err(err("penalty must be finite and positive"));
    }
    let mut kt = Vec::new();
    triplets_of(k1, 0, 0, 1.0, &mut kt);
    triplets_of(k2, n1, n1, 1.0, &mut kt);
    let k = build(n1 + n2, n1 + n2, &kt)?;
    let mut dt = Vec::new();
    triplets_of(b1, 0, 0, 1.0, &mut dt);
    triplets_of(b2, 0, n1, -1.0, &mut dt);
    let d = build(b1.nrows(), n1 + n2, &dt)?;
    let dtd = d.transpose().matmul(&d).map_err(|e| err(e.to_string()))?;
    let a = k.add_scaled(1.0, &dtd, penalty).map_err(|e| err(e.to_string()))?;
    let sym = symmetry(&a)?;
    let minimum = if let Ok((value, _)) = smallest_eigenvalue(&a, &LanczosOptions::default()) {
        value
    } else {
        let dense = DenseMatrix { nrows: a.nrows(), ncols: a.ncols(), data: a.to_dense() };
        eigvalsh(&dense).map_err(|e| err(e.to_string()))?.into_iter().fold(f64::INFINITY, f64::min)
    };
    Ok(SparseInterfaceSystem { matrix: a, symmetry_error: sym, minimum_eigenvalue: Some(minimum) })
}


pub fn symmetric_nitsche_operator(
    k: &DenseMatrix,
    jump: &DenseMatrix,
    flux: &DenseMatrix,
    measure: &[f64],
    penalty: &[f64],
) -> CaeResult<DenseMatrix> {
    let n = k.nrows;
    let m = jump.nrows;
    if n == 0
        || m == 0
        || k.ncols != n
        || jump.ncols != n
        || flux.nrows != m
        || flux.ncols != n
        || measure.len() != m
        || penalty.len() != m
        || k.data.len() != n * n
        || jump.data.len() != m * n
        || flux.data.len() != m * n
    {
        return Err(err("Nitsche dimensions invalid"));
    }
    if !k.data.iter().chain(&jump.data).chain(&flux.data).all(|v| v.is_finite())
        || !measure.iter().chain(penalty).all(|v| v.is_finite() && *v > 0.0)
    {
        return Err(err("Nitsche data must be finite with positive measures and penalties"));
    }
    let mut a = k.clone();
    for q in 0..m {
        for i in 0..n {
            for j in 0..n {
                let di = jump.get(q, i);
                let dj = jump.get(q, j);
                let fi = flux.get(q, i);
                let fj = flux.get(q, j);
                a.data[i * n + j] += measure[q] * (penalty[q] * di * dj - di * fj - fi * dj);
            }
        }
    }
    Ok(a)
}
