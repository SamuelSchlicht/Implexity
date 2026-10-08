// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![allow(clippy::needless_range_loop)]

use std::sync::{Arc, OnceLock};

use implexity_ad::AdError;
use implexity_linalg::lu::{CholeskySymbolic, Parallelism, SparseCholesky};
use implexity_linalg::{CscMatrix, CsrMatrix};

use super::ad::{A, Shape};

pub const XI_N: [f64; 8] = [-1.0, -1.0, -1.0, -1.0, 1.0, 1.0, 1.0, 1.0];
pub const ETA_N: [f64; 8] = [-1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0];
pub const ZETA_N: [f64; 8] = [-1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0];

#[must_use]
pub fn constitutive(nu: f64) -> [[f64; 6]; 6] {
    let lam = nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let mu = 1.0 / (2.0 * (1.0 + nu));
    let mut d = [[0.0; 6]; 6];
    for i in 0..3 {
        for j in 0..3 {
            d[i][j] = lam;
        }
        d[i][i] += 2.0 * mu;
        d[3 + i][3 + i] = mu;
    }
    d
}

#[must_use]
pub fn shape_gradients(xi: f64, eta: f64, zeta: f64, h: f64) -> [[f64; 8]; 3] {
    let s = 2.0 / h;
    let mut out = [[0.0; 8]; 3];
    for a in 0..8 {
        out[0][a] = s * (0.125 * XI_N[a] * (1.0 + eta * ETA_N[a]) * (1.0 + zeta * ZETA_N[a]));
        out[1][a] = s * (0.125 * ETA_N[a] * (1.0 + xi * XI_N[a]) * (1.0 + zeta * ZETA_N[a]));
        out[2][a] = s * (0.125 * ZETA_N[a] * (1.0 + xi * XI_N[a]) * (1.0 + eta * ETA_N[a]));
    }
    out
}

#[must_use]
pub fn strain_displacement(xi: f64, eta: f64, zeta: f64, h: f64) -> [[f64; 24]; 6] {
    let [dx, dy, dz] = shape_gradients(xi, eta, zeta, h);
    let mut b = [[0.0; 24]; 6];
    for a in 0..8 {
        b[0][3 * a] = dx[a];
        b[1][3 * a + 1] = dy[a];
        b[2][3 * a + 2] = dz[a];
        b[3][3 * a] = dy[a];
        b[3][3 * a + 1] = dx[a];
        b[4][3 * a + 1] = dz[a];
        b[4][3 * a + 2] = dy[a];
        b[5][3 * a] = dz[a];
        b[5][3 * a + 2] = dx[a];
    }
    b
}

fn gauss() -> [f64; 2] {
    let g = 1.0 / 3f64.sqrt();
    [-g, g]
}

#[must_use]
pub fn element_stiffness(nu: f64, h: f64) -> Vec<f64> {
    let d = constitutive(nu);
    let det_j = (h / 2.0).powi(3);
    let mut ke = vec![0.0; 576];
    for xi in gauss() {
        for eta in gauss() {
            for zeta in gauss() {
                let b = strain_displacement(xi, eta, zeta, h);
                let mut db = [[0.0; 24]; 6];
                for r in 0..6 {
                    for c in 0..24 {
                        let mut s = 0.0;
                        for k in 0..6 {
                            s += d[r][k] * b[k][c];
                        }
                        db[r][c] = s;
                    }
                }
                for i in 0..24 {
                    for j in 0..24 {
                        let mut s = 0.0;
                        for k in 0..6 {
                            s += b[k][i] * db[k][j];
                        }
                        ke[i * 24 + j] += s * det_j;
                    }
                }
            }
        }
    }
    let mut out = vec![0.0; 576];
    for i in 0..24 {
        for j in 0..24 {
            out[i * 24 + j] = 0.5 * (ke[i * 24 + j] + ke[j * 24 + i]);
        }
    }
    out
}

#[must_use]
pub fn element_conduction(h: f64) -> Vec<f64> {
    let det_j = (h / 2.0).powi(3);
    let mut ke = vec![0.0; 64];
    for xi in gauss() {
        for eta in gauss() {
            for zeta in gauss() {
                let b = shape_gradients(xi, eta, zeta, h);
                for i in 0..8 {
                    for j in 0..8 {
                        ke[i * 8 + j] += (b[0][i] * b[0][j] + b[1][i] * b[1][j] + b[2][i] * b[2][j]) * det_j;
                    }
                }
            }
        }
    }
    ke
}

#[must_use]
pub fn element_nodes(nelx: usize, nely: usize, nelz: usize) -> Vec<[usize; 8]> {
    let mut out = Vec::with_capacity(nelx * nely * nelz);
    for ex in 0..nelx {
        for ey in 0..nely {
            for ez in 0..nelz {
                out.push(std::array::from_fn(|a| {
                    let dx = usize::from(XI_N[a] > 0.0);
                    let dy = usize::from(ETA_N[a] > 0.0);
                    let dz = usize::from(ZETA_N[a] > 0.0);
                    ((ex + dx) * (nely + 1) + (ey + dy)) * (nelz + 1) + (ez + dz)
                }));
            }
        }
    }
    out
}

pub struct Pattern {
    pub n: usize,
    pub indptr: Vec<usize>,
    pub indices: Vec<usize>,
    pub slots: Vec<u32>,
    pub k: usize,
    pub diag: Vec<usize>,
    symbolic: OnceLock<Result<CholeskySymbolic, String>>,
}

impl std::fmt::Debug for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pattern(n={}, nnz={})", self.n, self.indices.len())
    }
}

impl Pattern {
    pub fn new(n: usize, edof: &[Vec<usize>], k: usize) -> Result<Self, String> {
        let mut rows: Vec<Vec<usize>> = vec![Vec::new(); n];
        for e in edof {
            for &i in e {
                rows[i].extend_from_slice(e);
            }
        }
        let mut indptr = Vec::with_capacity(n + 1);
        let mut indices = Vec::new();
        indptr.push(0);
        for (i, r) in rows.iter_mut().enumerate() {
            r.push(i);
            r.sort_unstable();
            r.dedup();
            indices.extend_from_slice(r);
            indptr.push(indices.len());
        }
        if indices.len() >= u32::MAX as usize {
            return Err("sparse pattern exceeds 32-bit slots".into());
        }
        let find = |i: usize, j: usize| -> usize {
            let row = &indices[indptr[i]..indptr[i + 1]];
            indptr[i] + row.partition_point(|&c| c < j)
        };
        let mut slots = Vec::with_capacity(edof.len() * k * k);
        for e in edof {
            for &i in e {
                for &j in e {
                    #[allow(clippy::cast_possible_truncation)]
                    slots.push(find(i, j) as u32);
                }
            }
        }
        let diag = (0..n).map(|i| find(i, i)).collect();
        Ok(Self { n, indptr, indices, slots, k, diag, symbolic: OnceLock::new() })
    }

    pub fn csc(&self, data: Vec<f64>) -> Result<CscMatrix, String> {
        CscMatrix::try_new(self.n, self.n, self.indptr.clone(), self.indices.clone(), data)
            .map_err(|e| e.to_string())
    }

    pub fn csr(&self, data: Vec<f64>) -> Result<CsrMatrix, String> {
        CsrMatrix::try_new(self.n, self.n, self.indptr.clone(), self.indices.clone(), data)
            .map_err(|e| e.to_string())
    }

    pub fn cholesky(&self, data: Vec<f64>) -> Result<SparseCholesky, String> {
        let a = self.csc(data)?;
        let sym = self
            .symbolic
            .get_or_init(|| CholeskySymbolic::analyze(&a).map_err(|e| e.to_string()))
            .as_ref()
            .map_err(Clone::clone)?;
        sym.factor(&a, Parallelism::Sequential).map_err(|e| e.to_string())
    }

    #[must_use]
    pub fn matvec(&self, data: &[f64], x: &[f64]) -> Vec<f64> {
        let mut y = vec![0.0; self.n];
        for (i, yi) in y.iter_mut().enumerate() {
            let mut s = 0.0;
            for p in self.indptr[i]..self.indptr[i + 1] {
                s += data[p] * x[self.indices[p]];
            }
            *yi = s;
        }
        y
    }
}

#[derive(Debug)]
pub struct MeshT {
    pub nelx: usize,
    pub nely: usize,
    pub nelz: usize,
    pub h: f64,
    pub ndof: usize,
    pub edof: Vec<[usize; 8]>,
    pub ke: Vec<f64>,
    pub ke_diag: Vec<f64>,
    pub pattern: Arc<Pattern>,
}

pub fn build_mesh_t(nelx: usize, nely: usize, nelz: usize, h: f64) -> Result<MeshT, String> {
    let ndof = (nelx + 1) * (nely + 1) * (nelz + 1);
    let edof = element_nodes(nelx, nely, nelz);
    let ke = element_conduction(h);
    let ke_diag = (0..8).map(|i| ke[i * 8 + i]).collect();
    let rows: Vec<Vec<usize>> = edof.iter().map(|e| e.to_vec()).collect();
    let pattern = Arc::new(Pattern::new(ndof, &rows, 8)?);
    Ok(MeshT { nelx, nely, nelz, h, ndof, edof, ke, ke_diag, pattern })
}

#[derive(Debug)]
pub struct Mesh3 {
    pub nelx: usize,
    pub nely: usize,
    pub nelz: usize,
    pub h: f64,
    pub nu: f64,
    pub ndof: usize,
    pub edof: Vec<[usize; 24]>,
    pub fixed: Vec<bool>,
    pub ke: Vec<f64>,
    pub ke_diag: Vec<f64>,
    pub pattern: Arc<Pattern>,
}

pub fn mech_pattern(nelx: usize, nely: usize, nelz: usize) -> Result<Arc<Pattern>, String> {
    let ndof = 3 * (nelx + 1) * (nely + 1) * (nelz + 1);
    let rows: Vec<Vec<usize>> = element_nodes(nelx, nely, nelz)
        .iter()
        .map(|e| e.iter().flat_map(|&n| (0..3).map(move |c| 3 * n + c)).collect())
        .collect();
    Ok(Arc::new(Pattern::new(ndof, &rows, 24)?))
}

pub fn build_mesh3(
    nelx: usize,
    nely: usize,
    nelz: usize,
    h: f64,
    nu: f64,
    fixed: &[usize],
    pattern: Option<Arc<Pattern>>,
) -> Result<Mesh3, String> {
    let ndof = 3 * (nelx + 1) * (nely + 1) * (nelz + 1);
    let edof: Vec<[usize; 24]> = element_nodes(nelx, nely, nelz)
        .iter()
        .map(|e| std::array::from_fn(|q| 3 * e[q / 3] + q % 3))
        .collect();
    let mut mask = vec![false; ndof];
    for &i in fixed {
        if i >= ndof {
            return Err(format!("fixed dof {i} out of range for {ndof} dofs"));
        }
        mask[i] = true;
    }
    let ke = element_stiffness(nu, h);
    let ke_diag = (0..24).map(|i| ke[i * 24 + i]).collect();
    let pattern = match pattern {
        Some(p) if p.n == ndof => p,
        _ => mech_pattern(nelx, nely, nelz)?,
    };
    Ok(Mesh3 { nelx, nely, nelz, h, nu, ndof, edof, fixed: mask, ke, ke_diag, pattern })
}

impl Mesh3 {
    #[must_use]
    pub fn apply(&self, e_elem: &[f64], u: &[f64]) -> Vec<f64> {
        let mut r = vec![0.0; self.ndof];
        for (e, dofs) in self.edof.iter().enumerate() {
            let ue: [f64; 24] = std::array::from_fn(|q| if self.fixed[dofs[q]] { 0.0 } else { u[dofs[q]] });
            for j in 0..24 {
                let mut s = 0.0;
                for i in 0..24 {
                    s += ue[i] * self.ke[i * 24 + j];
                }
                r[dofs[j]] += e_elem[e] * s;
            }
        }
        for (ri, &f) in r.iter_mut().zip(&self.fixed) {
            if f {
                *ri = 0.0;
            }
        }
        r
    }

    #[must_use]
    pub fn jacobi_diag(&self, e_elem: &[f64]) -> Vec<f64> {
        let mut d = vec![0.0; self.ndof];
        for (e, dofs) in self.edof.iter().enumerate() {
            for q in 0..24 {
                d[dofs[q]] += e_elem[e] * self.ke_diag[q];
            }
        }
        for (di, &f) in d.iter_mut().zip(&self.fixed) {
            if f {
                *di = 1.0;
            }
        }
        d
    }

    #[must_use]
    pub fn assemble(&self, e_elem: &[f64]) -> Vec<f64> {
        let p = &self.pattern;
        let mut data = vec![0.0; p.indices.len()];
        for (e, dofs) in self.edof.iter().enumerate() {
            let base = e * 576;
            for i in 0..24 {
                if self.fixed[dofs[i]] {
                    continue;
                }
                for j in 0..24 {
                    if self.fixed[dofs[j]] {
                        continue;
                    }
                    data[p.slots[base + i * 24 + j] as usize] += e_elem[e] * self.ke[i * 24 + j];
                }
            }
        }
        for (i, &f) in self.fixed.iter().enumerate() {
            if f {
                data[p.diag[i]] = 1.0;
            }
        }
        data
    }

    #[must_use]
    pub fn param_vjp(&self, x: &[f64], lam: &[f64]) -> Vec<f64> {
        self.edof
            .iter()
            .map(|dofs| {
                let xe: [f64; 24] =
                    std::array::from_fn(|q| if self.fixed[dofs[q]] { 0.0 } else { x[dofs[q]] });
                let le: [f64; 24] =
                    std::array::from_fn(|q| if self.fixed[dofs[q]] { 0.0 } else { lam[dofs[q]] });
                let mut s = 0.0;
                for i in 0..24 {
                    let mut t = 0.0;
                    for j in 0..24 {
                        t += self.ke[i * 24 + j] * xe[j];
                    }
                    s += le[i] * t;
                }
                s
            })
            .collect()
    }
}

impl MeshT {
    #[must_use]
    pub fn apply(&self, k_elem: &[f64], robin: &[f64], t: &[f64]) -> Vec<f64> {
        let mut r = vec![0.0; self.ndof];
        for (e, n) in self.edof.iter().enumerate() {
            let te: [f64; 8] = std::array::from_fn(|q| t[n[q]]);
            for j in 0..8 {
                let mut s = 0.0;
                for i in 0..8 {
                    s += te[i] * self.ke[i * 8 + j];
                }
                r[n[j]] += k_elem[e] * s;
            }
        }
        for i in 0..self.ndof {
            r[i] += robin[i] * t[i];
        }
        r
    }

    #[must_use]
    pub fn jacobi_diag(&self, k_elem: &[f64], robin: &[f64]) -> Vec<f64> {
        let mut d = vec![0.0; self.ndof];
        for (e, n) in self.edof.iter().enumerate() {
            for q in 0..8 {
                d[n[q]] += k_elem[e] * self.ke_diag[q];
            }
        }
        for i in 0..self.ndof {
            d[i] += robin[i];
        }
        d
    }

    #[must_use]
    pub fn assemble(&self, k_elem: &[f64], robin: &[f64], fixed: Option<&[bool]>) -> Vec<f64> {
        let p = &self.pattern;
        let mut data = vec![0.0; p.indices.len()];
        let is_fixed = |i: usize| fixed.is_some_and(|f| f[i]);
        for (e, n) in self.edof.iter().enumerate() {
            let base = e * 64;
            for i in 0..8 {
                if is_fixed(n[i]) {
                    continue;
                }
                for j in 0..8 {
                    if is_fixed(n[j]) {
                        continue;
                    }
                    data[p.slots[base + i * 8 + j] as usize] += k_elem[e] * self.ke[i * 8 + j];
                }
            }
        }
        for i in 0..self.ndof {
            if is_fixed(i) {
                data[p.diag[i]] = 1.0;
            } else {
                data[p.diag[i]] += robin[i];
            }
        }
        data
    }

    #[must_use]
    pub fn param_vjp(&self, x: &[f64], lam: &[f64], fixed: Option<&[bool]>) -> (Vec<f64>, Vec<f64>) {
        let free = |i: usize| !fixed.is_some_and(|f| f[i]);
        let kbar = self
            .edof
            .iter()
            .map(|n| {
                let xe: [f64; 8] = std::array::from_fn(|q| if free(n[q]) { x[n[q]] } else { 0.0 });
                let le: [f64; 8] = std::array::from_fn(|q| if free(n[q]) { lam[n[q]] } else { 0.0 });
                let mut s = 0.0;
                for i in 0..8 {
                    let mut t = 0.0;
                    for j in 0..8 {
                        t += self.ke[i * 8 + j] * xe[j];
                    }
                    s += le[i] * t;
                }
                s
            })
            .collect();
        let rbar = (0..self.ndof).map(|i| if free(i) { lam[i] * x[i] } else { 0.0 }).collect();
        (kbar, rbar)
    }
}

pub fn pcg(
    matvec: &dyn Fn(&[f64]) -> Vec<f64>,
    b: &[f64],
    x0: &[f64],
    minv: &[f64],
    tol: f64,
    maxiter: usize,
) -> (Vec<f64>, usize, f64) {
    let norm = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
    let dot = |a: &[f64], c: &[f64]| a.iter().zip(c).map(|(x, y)| x * y).sum::<f64>();
    let bnorm = norm(b);
    let denom = if bnorm > 0.0 { bnorm } else { 1.0 };
    let mut x = x0.to_vec();
    let ax = matvec(&x);
    let mut r: Vec<f64> = b.iter().zip(&ax).map(|(bi, ai)| bi - ai).collect();
    let mut z: Vec<f64> = r.iter().zip(minv).map(|(ri, mi)| mi * ri).collect();
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    let mut rn = norm(&r);
    let mut k = 0usize;
    while k < maxiter && rn > tol * denom {
        let ap = matvec(&p);
        let pap = dot(&p, &ap);
        let alpha = rz / if pap == 0.0 { 1.0 } else { pap };
        for i in 0..x.len() {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        for i in 0..z.len() {
            z[i] = minv[i] * r[i];
        }
        let rz_new = dot(&r, &z);
        let beta = rz_new / if rz == 0.0 { 1.0 } else { rz };
        for i in 0..p.len() {
            p[i] = z[i] + beta * p[i];
        }
        rz = rz_new;
        rn = norm(&r);
        k += 1;
    }
    let res = matvec(&x);
    let rel = norm(&b.iter().zip(&res).map(|(bi, ai)| bi - ai).collect::<Vec<_>>()) / denom;
    let valid =
        x.iter().all(|v| v.is_finite()) && rel.is_finite() && tol.is_finite() && tol >= 0.0 && rel <= tol;
    if !valid {
        x.fill(f64::NAN);
    }
    (x, k, rel)
}

#[must_use]
pub fn solve_probe3(
    mesh: &Mesh3,
    e_elem: &[f64],
    f: &[f64],
    tol: f64,
    maxiter: usize,
) -> (Vec<f64>, usize, f64) {
    let b: Vec<f64> = f.iter().zip(&mesh.fixed).map(|(v, &fx)| if fx { 0.0 } else { *v }).collect();
    let minv: Vec<f64> = mesh.jacobi_diag(e_elem).iter().map(|d| 1.0 / d).collect();
    pcg(&|v| mesh.apply(e_elem, v), &b, &vec![0.0; b.len()], &minv, tol, maxiter)
}

#[must_use]
pub fn solve_probe_t(
    mesh: &MeshT,
    k_elem: &[f64],
    robin: &[f64],
    q: &[f64],
    t0: Option<&[f64]>,
    tol: f64,
    maxiter: usize,
) -> (Vec<f64>, usize, f64) {
    let minv: Vec<f64> = mesh.jacobi_diag(k_elem, robin).iter().map(|d| 1.0 / d).collect();
    let start = t0.map_or_else(|| vec![0.0; q.len()], <[f64]>::to_vec);
    pcg(&|v| mesh.apply(k_elem, robin, v), q, &start, &minv, tol, maxiter)
}

fn relres(b: &[f64], ax: &[f64]) -> f64 {
    let nb = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nr = b.iter().zip(ax).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt();
    nr / if nb > 0.0 { nb } else { 1.0 }
}

fn checked(x: Vec<f64>, b: &[f64], ax: &[f64], tol: f64) -> Vec<f64> {
    let r = relres(b, ax);
    if x.iter().all(|v| v.is_finite()) && r.is_finite() && r <= tol.max(1e-10) {
        x
    } else {
        vec![f64::NAN; x.len()]
    }
}

#[must_use]
pub fn solve_temperature<'g>(
    mesh: &Arc<MeshT>,
    k_elem: A<'g>,
    robin: A<'g>,
    q: A<'g>,
    tol: f64,
    fixed: Option<(&[bool], &[f64])>,
) -> A<'g> {
    let g = q.graph();
    let n = mesh.ndof;
    match fixed {
        None => solve_t_core(mesh, k_elem, robin, q, None, tol),
        Some((mask, vals)) => {
            let mask: Arc<Vec<bool>> = Arc::new(mask.to_vec());
            let td: Vec<f64> = (0..n).map(|i| if mask[i] { vals[i] } else { 0.0 }).collect();
            let tdc = g.constant(td.clone(), Shape::d1(n));
            let ktd = apply_t_node(mesh, k_elem, robin, tdc);
            let rhs = q - ktd;
            let zero = g.scalar(0.0);
            let b = g.select(&mask, zero, rhs);
            let v = solve_t_core(mesh, k_elem, robin, b, Some(Arc::clone(&mask)), tol);
            let pv = g.select(&mask, zero, v);
            tdc + pv
        }
    }
}

#[must_use]
pub fn apply_t_node<'g>(mesh: &Arc<MeshT>, k_elem: A<'g>, robin: A<'g>, t: A<'g>) -> A<'g> {
    let g = t.graph();
    let (kv, rv, tv) = (k_elem.val(), robin.val(), t.val());
    let y = mesh.apply(&kv, &rv, &tv);
    let m = Arc::clone(mesh);
    let (kv, tv) = (Arc::new(kv), Arc::new(tv));
    let rv = Arc::new(rv);
    g.custom(
        &[k_elem, robin, t],
        y,
        Shape::d1(mesh.ndof),
        Box::new(move |gy: &[f64]| {
            let (kbar, rbar) = m.param_vjp(&tv, gy, None);
            let tbar = m.apply(&kv, &rv, gy);
            Ok(vec![kbar, rbar, tbar])
        }),
    )
}

fn solve_t_core<'g>(
    mesh: &Arc<MeshT>,
    k_elem: A<'g>,
    robin: A<'g>,
    b: A<'g>,
    fixed: Option<Arc<Vec<bool>>>,
    tol: f64,
) -> A<'g> {
    let g = b.graph();
    let (kv, rv, bv) = (k_elem.val(), robin.val(), b.val());
    let data = mesh.assemble(&kv, &rv, fixed.as_ref().map(|f| f.as_slice()));
    let fact = match mesh.pattern.cholesky(data) {
        Ok(f) => Arc::new(f),
        Err(e) => return g.failed(&format!("thermal solve: {e}"), Shape::d1(mesh.ndof)),
    };
    let mut x = bv.clone();
    if let Err(e) = fact.solve_in_place(&mut x) {
        return g.failed(&format!("thermal solve: {e}"), Shape::d1(mesh.ndof));
    }
    let ax = {
        let raw = mesh.apply(&kv, &rv, &x);
        match &fixed {
            Some(f) => {
                raw.iter().zip(f.iter()).zip(&x).map(|((a, &fx), xi)| if fx { *xi } else { *a }).collect()
            }
            None => raw,
        }
    };
    let x = checked(x, &bv, &ax, tol);
    let m = Arc::clone(mesh);
    let xs = Arc::new(x.clone());
    g.custom(
        &[k_elem, robin, b],
        x,
        Shape::d1(mesh.ndof),
        Box::new(move |gx: &[f64]| {
            let mut lam = gx.to_vec();
            if let Some(f) = &fixed {
                for (l, &fx) in lam.iter_mut().zip(f.iter()) {
                    if fx {
                        *l = 0.0;
                    }
                }
            }
            fact.solve_in_place(&mut lam).map_err(|e| AdError::Callback(e.to_string()))?;
            let (kb, rb) = m.param_vjp(&xs, &lam, fixed.as_ref().map(|f| f.as_slice()));
            Ok(vec![kb.iter().map(|v| -v).collect(), rb.iter().map(|v| -v).collect(), lam])
        }),
    )
}

#[must_use]
pub fn solve_displacement<'g>(mesh: &Arc<Mesh3>, e_elem: A<'g>, f: A<'g>, tol: f64) -> A<'g> {
    let g = f.graph();
    let fixed = Arc::new(mesh.fixed.clone());
    let zero = g.scalar(0.0);
    let b = g.select(&fixed, zero, f);
    let (ev, bv) = (e_elem.val(), b.val());
    let fact = match mesh.pattern.cholesky(mesh.assemble(&ev)) {
        Ok(f) => Arc::new(f),
        Err(e) => return g.failed(&format!("mechanical solve: {e}"), Shape::d1(mesh.ndof)),
    };
    let mut x = bv.clone();
    if let Err(e) = fact.solve_in_place(&mut x) {
        return g.failed(&format!("mechanical solve: {e}"), Shape::d1(mesh.ndof));
    }
    let ax: Vec<f64> = mesh
        .apply(&ev, &x)
        .iter()
        .zip(fixed.iter())
        .zip(&x)
        .map(|((a, &fx), xi)| if fx { *xi } else { *a })
        .collect();
    let x = checked(x, &bv, &ax, tol);
    let m = Arc::clone(mesh);
    let xs = Arc::new(x.clone());
    g.custom(
        &[e_elem, b],
        x,
        Shape::d1(mesh.ndof),
        Box::new(move |gx: &[f64]| {
            let mut lam: Vec<f64> =
                gx.iter().zip(fixed.iter()).map(|(v, &fx)| if fx { 0.0 } else { *v }).collect();
            fact.solve_in_place(&mut lam).map_err(|e| AdError::Callback(e.to_string()))?;
            let eb = m.param_vjp(&xs, &lam);
            Ok(vec![eb.iter().map(|v| -v).collect(), lam])
        }),
    )
}

#[must_use]
pub fn lump_element_to_nodes<'g>(field: A<'g>, edof: &Arc<Vec<[usize; 8]>>, ndof: usize) -> A<'g> {
    let e1 = Arc::clone(edof);
    let e2 = Arc::clone(edof);
    let nel = edof.len();
    field.graph().linear(
        field.flat(),
        Shape::d1(ndof),
        move |v| {
            let mut out = vec![0.0; ndof];
            for (e, n) in e1.iter().enumerate() {
                let c = v[e] / 8.0;
                for &q in n {
                    out[q] += c;
                }
            }
            out
        },
        move |g| {
            let mut out = vec![0.0; nel];
            for (e, n) in e2.iter().enumerate() {
                out[e] = n.iter().map(|&q| g[q]).sum::<f64>() / 8.0;
            }
            out
        },
    )
}

#[must_use]
pub fn element_average<'g>(t: A<'g>, edof: &Arc<Vec<[usize; 8]>>, shape: [usize; 3]) -> A<'g> {
    let e1 = Arc::clone(edof);
    let e2 = Arc::clone(edof);
    let ndof = t.len();
    t.graph().linear(
        t,
        Shape::d3(shape),
        move |v| e1.iter().map(|n| n.iter().map(|&q| v[q]).sum::<f64>() / 8.0).collect(),
        move |g| {
            let mut out = vec![0.0; ndof];
            for (e, n) in e2.iter().enumerate() {
                let c = g[e] / 8.0;
                for &q in n {
                    out[q] += c;
                }
            }
            out
        },
    )
}

#[must_use]
pub fn face_nodes(nelx: usize, nely: usize, nelz: usize, axis: usize, high: bool) -> Vec<[usize; 4]> {
    let nid = |ix: usize, iy: usize, iz: usize| (ix * (nely + 1) + iy) * (nelz + 1) + iz;
    let d = [[0, 0], [0, 1], [1, 0], [1, 1]];
    let mut out = Vec::new();
    match axis {
        0 => {
            let ix = if high { nelx } else { 0 };
            for a in 0..nely {
                for b in 0..nelz {
                    out.push(std::array::from_fn(|q| nid(ix, a + d[q][0], b + d[q][1])));
                }
            }
        }
        1 => {
            let iy = if high { nely } else { 0 };
            for a in 0..nelx {
                for b in 0..nelz {
                    out.push(std::array::from_fn(|q| nid(a + d[q][0], iy, b + d[q][1])));
                }
            }
        }
        _ => {
            let iz = if high { nelz } else { 0 };
            for a in 0..nelx {
                for b in 0..nely {
                    out.push(std::array::from_fn(|q| nid(a + d[q][0], b + d[q][1], iz)));
                }
            }
        }
    }
    out
}
