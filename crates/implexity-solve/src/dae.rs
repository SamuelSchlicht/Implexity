// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseLu, DenseMatrix, matrix_rank, null_space};
use serde_json::{Value, json};

use crate::certificate::norm2;

fn lin(e: &implexity_linalg::error::LinalgError) -> CaeError {
    CaeError::contract(e.to_string())
}

#[must_use]
pub fn kkt(k: &DenseMatrix, c: &DenseMatrix) -> DenseMatrix {
    let n = k.nrows;
    let m = c.nrows;
    let size = n + m;
    let mut a = DenseMatrix::zeros(size, size);
    for i in 0..n {
        a.data[i * size..i * size + n].copy_from_slice(&k.data[i * n..(i + 1) * n]);
    }
    for r in 0..m {
        for j in 0..n {
            let v = c.data[r * n + j];
            a.data[(n + r) * size + j] = v;
            a.data[j * size + n + r] = v;
        }
    }
    a
}

fn solve(a: &DenseMatrix, b: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
    DenseLu::new(a).and_then(|lu| lu.solve(b, 1, transpose)).map_err(|e| lin(&e))
}

fn residual(a: &DenseMatrix, z: &[f64], b: &[f64]) -> CaeResult<Vec<f64>> {
    let az = a.matvec(z).map_err(|e| lin(&e))?;
    Ok(az.iter().zip(b).map(|(p, q)| p - q).collect())
}

#[derive(Clone, Debug, PartialEq)]
pub struct SaddlePointResult {
    pub state: Vec<f64>,
    pub multiplier: Vec<f64>,
    pub residual_norm: f64,
    pub constraint_norm: f64,
    pub rank: usize,
}



pub fn solve_saddle_point(
    k: &DenseMatrix,
    c: &DenseMatrix,
    f: &[f64],
    g: Option<&[f64]>,
    rcond: f64,
) -> CaeResult<SaddlePointResult> {
    if k.nrows != k.ncols || f.len() != k.nrows {
        return Err(CaeError::contract("K/f dimensions invalid"));
    }
    if c.ncols != k.nrows {
        return Err(CaeError::contract("C dimensions invalid"));
    }
    let g = g.map_or_else(|| vec![0.0; c.nrows], <[f64]>::to_vec);
    if g.len() != c.nrows {
        return Err(CaeError::contract("C dimensions invalid"));
    }
    let a = kkt(k, c);
    let b: Vec<f64> = f.iter().chain(&g).copied().collect();
    let tol = rcond * a.norm2().map_err(|e| lin(&e))?;
    let rank = matrix_rank(&a, Some(tol)).map_err(|e| lin(&e))?;
    if rank < a.nrows {
        return Err(CaeError::contract(
            "saddle-point system is rank deficient; gauge/nullspace treatment required",
        ));
    }
    let z = solve(&a, &b, false)?;
    let n = k.nrows;
    let res = residual(&a, &z, &b)?;
    Ok(SaddlePointResult {
        state: z[..n].to_vec(),
        multiplier: z[n..].to_vec(),
        residual_norm: norm2(&res[..n]),
        constraint_norm: norm2(&res[n..]),
        rank,
    })
}

fn kkt_adjoint(
    k: &DenseMatrix,
    c: &DenseMatrix,
    q: &[f64],
    qc: Option<&[f64]>,
    check_rank: bool,
    message: &str,
) -> CaeResult<(Vec<f64>, Vec<f64>)> {
    let qc = qc.map_or_else(|| vec![0.0; c.nrows], <[f64]>::to_vec);
    if q.len() != k.nrows || qc.len() != c.nrows {
        return Err(CaeError::contract("adjoint right-hand side dimensions invalid"));
    }
    let a = kkt(k, c);
    if check_rank && matrix_rank(&a, None).map_err(|e| lin(&e))? < a.nrows {
        return Err(CaeError::contract(message.to_string()));
    }
    let rhs: Vec<f64> = q.iter().chain(&qc).copied().collect();
    let z = solve(&a, &rhs, true)?;
    Ok((z[..k.nrows].to_vec(), z[k.nrows..].to_vec()))
}



pub fn solve_saddle_adjoint(
    k: &DenseMatrix,
    c: &DenseMatrix,
    q: &[f64],
    qc: Option<&[f64]>,
) -> CaeResult<(Vec<f64>, Vec<f64>)> {
    kkt_adjoint(k, c, q, qc, true, "adjoint saddle system is rank deficient")
}



pub fn nullspace_diagnostics(c: &DenseMatrix, tol: f64) -> CaeResult<(usize, usize, DenseMatrix)> {
    let z = null_space(c, Some(tol)).map_err(|e| lin(&e))?;
    let rank = matrix_rank(c, Some(tol)).map_err(|e| lin(&e))?;
    Ok((rank, z.ncols, z))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConstrainedAccelerationResult {
    pub acceleration: Vec<f64>,
    pub multiplier: Vec<f64>,
    pub position_constraint_norm: f64,
    pub velocity_constraint_norm: f64,
    pub acceleration_constraint_norm: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ConstraintData<'a> {
    pub q: Option<&'a [f64]>,
    pub v: Option<&'a [f64]>,
    pub g: Option<&'a [f64]>,
    pub gdot: Option<&'a [f64]>,
    pub gddot: Option<&'a [f64]>,
}



pub fn solve_index2_acceleration(
    m: &DenseMatrix,
    c: &DenseMatrix,
    force: &[f64],
    data: ConstraintData<'_>,
) -> CaeResult<ConstrainedAccelerationResult> {
    let n = m.nrows;
    if m.ncols != n || c.ncols != n || force.len() != n {
        return Err(CaeError::contract("invalid M/C/force dimensions"));
    }
    let k = c.nrows;
    let rhs_c = data.gddot.map_or_else(|| vec![0.0; k], <[f64]>::to_vec);
    let a = kkt(m, c);
    if matrix_rank(&a, None).map_err(|e| lin(&e))? < a.nrows {
        return Err(CaeError::contract(
            "index-2 saddle system singular; gauge/redundant constraints must be resolved",
        ));
    }
    let b: Vec<f64> = force.iter().chain(&rhs_c).copied().collect();
    let z = solve(&a, &b, false)?;
    let acc = z[..n].to_vec();
    let lam = z[n..].to_vec();
    let norm_of = |x: &[f64], t: &[f64]| -> CaeResult<f64> {
        let cx = c.matvec(x).map_err(|e| lin(&e))?;
        Ok(norm2(&cx.iter().zip(t).map(|(p, q)| p - q).collect::<Vec<_>>()))
    };
    let pn = match (data.q, data.g) {
        (Some(q), Some(g)) => norm_of(q, g)?,
        _ => 0.0,
    };
    let vn = match (data.v, data.gdot) {
        (Some(v), Some(gd)) => norm_of(v, gd)?,
        _ => 0.0,
    };
    let an = norm_of(&acc, &rhs_c)?;
    Ok(ConstrainedAccelerationResult {
        acceleration: acc,
        multiplier: lam,
        position_constraint_norm: pn,
        velocity_constraint_norm: vn,
        acceleration_constraint_norm: an,
    })
}



pub fn index2_adjoint(
    m: &DenseMatrix,
    c: &DenseMatrix,
    q: &[f64],
    qc: Option<&[f64]>,
) -> CaeResult<(Vec<f64>, Vec<f64>)> {
    kkt_adjoint(m, c, q, qc, true, "adjoint index-2 system singular")
}



pub fn constraint_index_diagnostics(
    c: &DenseMatrix,
    cdot: Option<&DenseMatrix>,
    tol: f64,
) -> CaeResult<Value> {
    let r = matrix_rank(c, Some(tol)).map_err(|e| lin(&e))?;
    let mut out = json!({
        "constraint_rank": r,
        "constraint_count": c.nrows,
        "redundant_constraints": c.nrows - r,
        "requires_index_reduction": true,
    });
    if let (Some(cd), Value::Object(map)) = (cdot, &mut out) {
        map.insert("time_varying_constraints".into(), json!(cd.norm_fro() > tol));
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChainSolve {
    pub state: Vec<f64>,
    pub multipliers: Vec<f64>,
    pub constraint_residual: f64,
    pub rank: usize,
}

fn stack(constraints: &[DenseMatrix], n: usize) -> CaeResult<DenseMatrix> {
    let mut data = Vec::new();
    let mut rows = 0;
    for c in constraints {
        if c.ncols != n {
            return Err(CaeError::contract("invalid state system"));
        }
        data.extend_from_slice(&c.data);
        rows += c.nrows;
    }
    Ok(DenseMatrix { nrows: rows, ncols: n, data })
}



pub fn solve_constraint_chain(
    h: &DenseMatrix,
    constraints: &[DenseMatrix],
    rhs: &[f64],
    targets: &[Vec<f64>],
) -> CaeResult<ChainSolve> {
    let n = h.nrows;
    if h.ncols != n || rhs.len() != n {
        return Err(CaeError::contract("invalid state system"));
    }
    let c = stack(constraints, n)?;
    let t: Vec<f64> =
        if constraints.is_empty() { Vec::new() } else { targets.iter().flatten().copied().collect() };
    if c.nrows != t.len() {
        return Err(CaeError::contract("constraint target mismatch"));
    }
    let rank = if c.nrows == 0 { 0 } else { matrix_rank(&c, None).map_err(|e| lin(&e))? };
    if rank < c.nrows {
        return Err(CaeError::contract("redundant higher-index constraint chain"));
    }
    let k = kkt(h, &c);
    if matrix_rank(&k, None).map_err(|e| lin(&e))? < k.nrows {
        return Err(CaeError::contract("singular reduced DAE system"));
    }
    let b: Vec<f64> = rhs.iter().chain(&t).copied().collect();
    let z = solve(&k, &b, false)?;
    let x = z[..n].to_vec();
    let cx = if c.nrows == 0 { Vec::new() } else { c.matvec(&x).map_err(|e| lin(&e))? };
    let res: Vec<f64> = cx.iter().zip(&t).map(|(p, q)| p - q).collect();
    Ok(ChainSolve { state: x, multipliers: z[n..].to_vec(), constraint_residual: norm2(&res), rank })
}



pub fn chain_adjoint(
    h: &DenseMatrix,
    constraints: &[DenseMatrix],
    q: &[f64],
    qc: Option<&[f64]>,
) -> CaeResult<(Vec<f64>, Vec<f64>)> {
    let c = stack(constraints, h.nrows)?;
    kkt_adjoint(h, &c, q, qc, false, "")
}

