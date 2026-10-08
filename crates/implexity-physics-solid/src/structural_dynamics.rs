// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use serde_json::{Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::CsrMatrix;

use crate::util::contract;

fn linalg<T>(r: Result<T, implexity_linalg::LinalgError>) -> Result<T, CaeError> {
    r.map_err(|e| CaeError::contract(e.to_string()))
}


pub fn conforming_interface_map(fluid: &[[f64; 3]], solid: &[[f64; 3]]) -> Result<Vec<usize>, CaeError> {
    for (name, a) in [("fluid", fluid), ("solid", solid)] {
        if a.is_empty() || a.iter().flatten().any(|v| !v.is_finite()) {
            return contract(format!(
                "{name} interface nodes require finite numerical [node,3] metre coordinates"
            ));
        }
    }
    let key = |p: &[f64; 3]| p.map(|v| if v == 0.0 { 0.0_f64.to_bits() } else { v.to_bits() });
    let mut lookup: BTreeMap<[u64; 3], usize> = BTreeMap::new();
    for (i, p) in solid.iter().enumerate() {
        lookup.insert(key(p), i);
    }
    if lookup.len() != solid.len() {
        return contract("duplicate solid nodes make interface binding ambiguous");
    }
    fluid
        .iter()
        .map(|p| {
            lookup.get(&key(p)).copied().ok_or_else(|| {
                CaeError::contract("nonconforming interface: a fluid load node has no exact solid node")
            })
        })
        .collect()
}

#[must_use]
pub fn transfer_conforming_forces<S: Scalar>(
    forces: &[[S; 3]],
    indices: &[usize],
    count: usize,
) -> Vec<[S; 3]> {
    let mut out = vec![[S::zero(); 3]; count];
    for (f, i) in forces.iter().zip(indices) {
        for c in 0..3 {
            out[*i][c] += f[c];
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct TetMatrices<S> {
    pub mass: Vec<S>,
    pub stiffness: Vec<S>,
    pub stress: Vec<S>,
    pub volume: S,
}

fn inv4<S: Scalar>(m: &[[S; 4]; 4]) -> [[S; 4]; 4] {

    let mut a: Vec<Vec<S>> = m.iter().map(|r| r.to_vec()).collect();
    let mut inv: Vec<Vec<S>> =
        (0..4).map(|i| (0..4).map(|j| if i == j { S::one() } else { S::zero() }).collect()).collect();
    for col in 0..4 {
        let pivot = (col..4)
            .max_by(|&i, &j| a[i][col].value().abs().total_cmp(&a[j][col].value().abs()))
            .unwrap_or(col);
        a.swap(col, pivot);
        inv.swap(col, pivot);
        let p = a[col][col];
        for j in 0..4 {
            a[col][j] /= p;
            inv[col][j] /= p;
        }
        for row in 0..4 {
            if row != col {
                let f = a[row][col];
                for j in 0..4 {
                    a[row][j] = a[row][j] - f * a[col][j];
                    inv[row][j] = inv[row][j] - f * inv[col][j];
                }
            }
        }
    }
    std::array::from_fn(|i| std::array::from_fn(|j| inv[i][j]))
}

fn det3<S: Scalar>(m: [[S; 3]; 3]) -> S {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

pub fn tetrahedron_matrices<S: Scalar>(
    points: &[[S; 3]; 4],
    density: S,
    young: S,
    poisson: S,
) -> TetMatrices<S> {
    let affine: [[S; 4]; 4] = std::array::from_fn(|i| [S::one(), points[i][0], points[i][1], points[i][2]]);
    let inv = inv4(&affine);
    let gradients: [[S; 3]; 4] = std::array::from_fn(|i| std::array::from_fn(|a| inv[a + 1][i]));
    let edges: [[S; 3]; 3] =
        std::array::from_fn(|r| std::array::from_fn(|c| points[r + 1][c] - points[0][c]));
    let volume = det3(edges) / 6.0;
    let mut b = [S::zero(); 6 * 12];
    for (i, g) in gradients.iter().enumerate() {
        let (gx, gy, gz) = (g[0], g[1], g[2]);
        b[3 * i] = gx;
        b[12 + 3 * i + 1] = gy;
        b[24 + 3 * i + 2] = gz;
        b[36 + 3 * i] = gy;
        b[36 + 3 * i + 1] = gx;
        b[48 + 3 * i + 1] = gz;
        b[48 + 3 * i + 2] = gy;
        b[60 + 3 * i] = gz;
        b[60 + 3 * i + 2] = gx;
    }
    let mu = young / ((poisson + 1.0) * 2.0);
    let lam = young * poisson / ((poisson + 1.0) * (-(poisson * 2.0) + 1.0));
    let mut d = [S::zero(); 36];
    for i in 0..6 {
        d[i * 6 + i] = mu;
    }
    for i in 0..3 {
        for j in 0..3 {
            d[i * 6 + j] = if i == j { lam + mu * 2.0 } else { lam };
        }
    }
    let mut db = vec![S::zero(); 72];
    for i in 0..6 {
        for j in 0..12 {
            let mut acc = S::zero();
            for k in 0..6 {
                acc += d[i * 6 + k] * b[k * 12 + j];
            }
            db[i * 12 + j] = acc;
        }
    }
    let mut stiffness = vec![S::zero(); 144];
    for i in 0..12 {
        for j in 0..12 {
            let mut acc = S::zero();
            for k in 0..6 {
                acc += b[k * 12 + i] * db[k * 12 + j];
            }
            stiffness[i * 12 + j] = volume * acc;
        }
    }
    let mut mass = vec![S::zero(); 144];
    let factor = density * volume / 20.0;
    for a in 0..4 {
        for bnode in 0..4 {
            let w = if a == bnode { 2.0 } else { 1.0 };
            for c in 0..3 {
                mass[(3 * a + c) * 12 + 3 * bnode + c] = factor * w;
            }
        }
    }
    TetMatrices { mass, stiffness, stress: db, volume }
}


pub fn require_positive_reference_volumes(volumes: &[f64], caller: &str) -> Result<(), CaeError> {
    if volumes.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        let min = volumes.iter().copied().fold(f64::INFINITY, f64::min);
        return contract(format!(
            "{caller}: inverted or degenerate reference tetrahedron (minimum signed volume {} m^3); orient the mesh positively or route through structural_dynamics.assemble_linear_tetrahedra, which validates orientation",
            crate::solid_history::format_e3(min)
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LinearAssembly {
    pub mass: CsrMatrix,
    pub stiffness: CsrMatrix,
    pub stress: CsrMatrix,
    pub volumes: Vec<f64>,
}


pub fn assemble_linear_tetrahedra(
    points: &[[f64; 3]],
    elements: &[[usize; 4]],
    density: &[f64],
    young: &[f64],
    poisson: &[f64],
) -> Result<LinearAssembly, CaeError> {
    if points.iter().flatten().any(|v| !v.is_finite()) {
        return contract("points require finite numerical [node,3] metre coordinates");
    }
    if elements.is_empty() {
        return contract("elements require integer [element,4] node indices");
    }
    let mut used = vec![false; points.len()];
    for t in elements {
        for n in t {
            if *n >= points.len() {
                return contract("invalid connectivity or unused mesh nodes");
            }
            used[*n] = true;
        }
    }
    if used.iter().any(|u| !u) {
        return contract("invalid connectivity or unused mesh nodes");
    }
    let ne = elements.len();
    for (name, a) in [("density", density), ("Young modulus", young), ("Poisson ratio", poisson)] {
        if a.len() != ne || a.iter().any(|v| !v.is_finite()) {
            return contract(format!("{name} requires one finite real value per tetrahedron"));
        }
    }
    if density.iter().any(|v| *v <= 0.0)
        || young.iter().any(|v| *v <= 0.0)
        || poisson.iter().any(|v| *v <= -1.0 || *v >= 0.5)
    {
        return contract("positive density/E and -1 < nu < 0.5 required");
    }
    let mut volumes = Vec::with_capacity(ne);
    for t in elements {
        let edges: [[f64; 3]; 3] =
            std::array::from_fn(|r| std::array::from_fn(|c| points[t[r + 1]][c] - points[t[0]][c]));
        let det = det3(edges);
        let scale = edges
            .iter()
            .map(|e| (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt())
            .fold(0.0, f64::max)
            .powi(3);
        if det <= 1e-12 * scale {
            return contract("inverted or numerically degenerate tetrahedron");
        }
        volumes.push(det / 6.0);
    }
    let ndof = 3 * points.len();
    let (mut rows, mut cols, mut mv, mut kv) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut sr, mut sc, mut sv) = (Vec::new(), Vec::new(), Vec::new());
    for (e, t) in elements.iter().enumerate() {
        let xyz: [[f64; 3]; 4] = std::array::from_fn(|i| points[t[i]]);
        let m = tetrahedron_matrices(&xyz, density[e], young[e], poisson[e]);
        let dofs: Vec<usize> = t.iter().flat_map(|n| (0..3).map(move |c| 3 * n + c)).collect();
        for i in 0..12 {
            for j in 0..12 {
                rows.push(dofs[i]);
                cols.push(dofs[j]);
                mv.push(m.mass[i * 12 + j]);
                kv.push(m.stiffness[i * 12 + j]);
            }
        }
        for i in 0..6 {
            for j in 0..12 {
                sr.push(6 * e + i);
                sc.push(dofs[j]);
                sv.push(m.stress[i * 12 + j]);
            }
        }
    }
    Ok(LinearAssembly {
        mass: linalg(CsrMatrix::from_triplets(ndof, ndof, &rows, &cols, &mv))?,
        stiffness: linalg(CsrMatrix::from_triplets(ndof, ndof, &rows, &cols, &kv))?,
        stress: linalg(CsrMatrix::from_triplets(6 * ne, ndof, &sr, &sc, &sv))?,
        volumes,
    })
}

#[derive(Debug, Clone)]
pub struct DenseAssembly<S> {
    pub mass: Vec<S>,
    pub stiffness: Vec<S>,
    pub stress: Vec<S>,
    pub volumes: Vec<S>,
}


pub fn differentiable_dense_tetrahedra<S: Scalar>(
    points: &[[S; 3]],
    elements: &[[usize; 4]],
    density: &[S],
    young: &[S],
    poisson: &[S],
) -> Result<DenseAssembly<S>, CaeError> {
    if points.len() > 256 {
        return contract("dense differentiated assembly is limited to 256 nodes");
    }
    let ndof = 3 * points.len();
    let ne = elements.len();
    let mut out = DenseAssembly {
        mass: vec![S::zero(); ndof * ndof],
        stiffness: vec![S::zero(); ndof * ndof],
        stress: vec![S::zero(); 6 * ne * ndof],
        volumes: Vec::with_capacity(ne),
    };
    let mut concrete = Vec::with_capacity(ne);
    for (e, t) in elements.iter().enumerate() {
        let xyz: [[S; 3]; 4] = std::array::from_fn(|i| points[t[i]]);
        let m = tetrahedron_matrices(&xyz, density[e], young[e], poisson[e]);
        concrete.push(m.volume.value());
        let dofs: Vec<usize> = t.iter().flat_map(|n| (0..3).map(move |c| 3 * n + c)).collect();
        for i in 0..12 {
            for j in 0..12 {
                out.mass[dofs[i] * ndof + dofs[j]] += m.mass[i * 12 + j];
                out.stiffness[dofs[i] * ndof + dofs[j]] += m.stiffness[i * 12 + j];
            }
        }
        for i in 0..6 {
            for j in 0..12 {
                out.stress[(6 * e + i) * ndof + dofs[j]] += m.stress[i * 12 + j];
            }
        }
        out.volumes.push(m.volume);
    }
    require_positive_reference_volumes(&concrete, "structural_dynamics.differentiable_dense_tetrahedra")?;
    Ok(out)
}


pub fn recover_linear_stress_history(stress: &CsrMatrix, history: &[Vec<f64>]) -> Result<Value, CaeError> {
    let (rows, cols) = stress.shape();
    if history.is_empty()
        || history.iter().any(|r| r.len() != history[0].len() || r.iter().any(|v| !v.is_finite()))
        || history[0].is_empty()
    {
        return contract("displacements require finite [time,full_node_dof] values");
    }
    if cols != history[0].len() || rows == 0 || rows % 6 != 0 {
        return contract("stress operator requires six rows per element and every full displacement DOF");
    }
    if !stress.is_finite() {
        return contract("stress operator requires finite real coefficients");
    }
    let mut physical = Vec::new();
    let mut mandel = Vec::new();
    for u in history {
        let s = linalg(stress.matvec(u))?;
        if s.iter().any(|v| !v.is_finite()) {
            return contract("nonfinite recovered stress");
        }
        let p: Vec<Vec<f64>> = s.chunks(6).map(<[f64]>::to_vec).collect();
        let m: Vec<Vec<f64>> = p
            .iter()
            .map(|r| {
                vec![
                    r[0],
                    r[1],
                    r[2],
                    r[4] * std::f64::consts::SQRT_2,
                    r[5] * std::f64::consts::SQRT_2,
                    r[3] * std::f64::consts::SQRT_2,
                ]
            })
            .collect();
        physical.push(p);
        mandel.push(m);
    }
    Ok(json!({"stress_physical_Pa": physical, "stress_mandel_Pa": mandel,
        "physical_order": ["xx", "yy", "zz", "xy", "yz", "xz"],
        "mandel_order": ["xx", "yy", "zz", "sqrt2_yz", "sqrt2_xz", "sqrt2_xy"]}))
}

fn solve(a: &DenseMatrix, b: &[f64]) -> Result<Vec<f64>, CaeError> {
    linalg(implexity_linalg::dense::solve(a, b, 1))
}

fn matvec(a: &DenseMatrix, x: &[f64]) -> Vec<f64> {
    a.matvec(x).unwrap_or_default()
}

fn axpy(a: &[f64], b: &[f64], alpha: f64) -> Vec<f64> {
    a.iter().zip(b).map(|(x, y)| x + alpha * y).collect()
}

fn dense_sum(a: &DenseMatrix, b: &DenseMatrix, alpha: f64, c: &DenseMatrix, beta: f64) -> DenseMatrix {
    let data = a.data.iter().zip(&b.data).zip(&c.data).map(|((x, y), z)| x + alpha * y + beta * z).collect();
    DenseMatrix { nrows: a.nrows, ncols: a.ncols, data }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewmarkHistory {
    pub displacement: Vec<Vec<f64>>,
    pub velocity: Vec<Vec<f64>>,
    pub acceleration: Vec<Vec<f64>>,
    pub acceleration_end: Vec<Vec<f64>>,
}


pub fn average_acceleration_history(
    m: &DenseMatrix,
    c: &DenseMatrix,
    k: &DenseMatrix,
    forces: &[Vec<f64>],
    times: &[f64],
    u0: &[f64],
    v0: &[f64],
) -> Result<NewmarkHistory, CaeError> {
    let rhs0: Vec<f64> =
        forces[0].iter().zip(matvec(c, v0)).zip(matvec(k, u0)).map(|((f, cv), ku)| f - cv - ku).collect();
    let a0 = solve(m, &rhs0)?;
    let mut h = NewmarkHistory {
        displacement: vec![u0.to_vec()],
        velocity: vec![v0.to_vec()],
        acceleration: vec![a0],
        ..NewmarkHistory::default()
    };
    for n in 1..times.len() {
        let dt = times[n] - times[n - 1];
        let (u, v) = (h.displacement[n - 1].clone(), h.velocity[n - 1].clone());
        let effective = dense_sum(m, c, 0.5 * dt, k, 0.25 * dt * dt);
        let mv = matvec(m, &v);
        let ku = matvec(k, &u);
        let rhs: Vec<f64> = (0..u.len())
            .map(|i| dt * mv[i] + 0.25 * dt * dt * (forces[n - 1][i] + forces[n][i] - 2.0 * ku[i]))
            .collect();
        let du = solve(&effective, &rhs)?;
        let un = axpy(&u, &du, 1.0);
        let vn: Vec<f64> = du.iter().zip(&v).map(|(d, v)| 2.0 * d / dt - v).collect();
        let r: Vec<f64> = forces[n]
            .iter()
            .zip(matvec(c, &vn))
            .zip(matvec(k, &un))
            .map(|((f, cv), ku)| f - cv - ku)
            .collect();
        let an = solve(m, &r)?;
        h.displacement.push(un);
        h.velocity.push(vn);
        h.acceleration.push(an);
    }
    Ok(h)
}


pub fn step_force_history(
    m: &DenseMatrix,
    c: &DenseMatrix,
    k: &DenseMatrix,
    interval_forces: &[Vec<f64>],
    times: &[f64],
    u0: &[f64],
    v0: &[f64],
) -> Result<(NewmarkHistory, Value), CaeError> {
    let mut h = NewmarkHistory {
        displacement: vec![u0.to_vec()],
        velocity: vec![v0.to_vec()],
        ..NewmarkHistory::default()
    };
    for n in 0..times.len() - 1 {
        let dt = times[n + 1] - times[n];
        let (u, v) = (h.displacement[n].clone(), h.velocity[n].clone());
        let force = &interval_forces[n];
        let start: Vec<f64> =
            force.iter().zip(matvec(c, &v)).zip(matvec(k, &u)).map(|((f, cv), ku)| f - cv - ku).collect();
        let start_a = solve(m, &start)?;
        let effective = dense_sum(m, c, 0.5 * dt, k, 0.25 * dt * dt);
        let mv = matvec(m, &v);
        let ku = matvec(k, &u);
        let rhs: Vec<f64> = (0..u.len()).map(|i| dt * mv[i] + 0.5 * dt * dt * (force[i] - ku[i])).collect();
        let du = solve(&effective, &rhs)?;
        let un = axpy(&u, &du, 1.0);
        let vn: Vec<f64> = du.iter().zip(&v).map(|(d, v)| 2.0 * d / dt - v).collect();
        let end: Vec<f64> =
            force.iter().zip(matvec(c, &vn)).zip(matvec(k, &un)).map(|((f, cv), ku)| f - cv - ku).collect();
        let end_a = solve(m, &end)?;
        h.displacement.push(un);
        h.velocity.push(vn);
        h.acceleration.push(start_a);
        h.acceleration_end.push(end_a);
    }
    let quad = |a: &DenseMatrix, x: &[f64]| x.iter().zip(matvec(a, x)).map(|(p, q)| p * q).sum::<f64>();
    let energy: Vec<f64> =
        h.velocity.iter().zip(&h.displacement).map(|(v, u)| 0.5 * quad(m, v) + 0.5 * quad(k, u)).collect();
    let mut work = Vec::new();
    let mut dissipation = Vec::new();
    let mut impulse = Vec::new();
    for n in 0..times.len() - 1 {
        let dt = times[n + 1] - times[n];
        let du: Vec<f64> = h.displacement[n + 1].iter().zip(&h.displacement[n]).map(|(a, b)| a - b).collect();
        work.push(interval_forces[n].iter().zip(&du).map(|(f, d)| f * d).sum::<f64>());
        let av: Vec<f64> = h.velocity[n + 1].iter().zip(&h.velocity[n]).map(|(a, b)| 0.5 * (a + b)).collect();
        dissipation.push(dt * quad(c, &av));
        impulse.push(interval_forces[n].iter().map(|f| dt * f).collect::<Vec<_>>());
    }
    let balance: Vec<f64> =
        (0..work.len()).map(|n| energy[n + 1] - energy[n] - work[n] + dissipation[n]).collect();
    Ok((
        h,
        json!({"external_impulse_Ns": impulse, "mechanical_energy_J": energy, "external_work_J": work,
        "damping_dissipation_J": dissipation, "energy_balance_error_J": balance}),
    ))
}

#[must_use]
pub fn energy_ledger(
    h: &NewmarkHistory,
    m: &DenseMatrix,
    c: &DenseMatrix,
    k: &DenseMatrix,
    forces: &[Vec<f64>],
    times: &[f64],
) -> Value {
    let quad = |a: &DenseMatrix, x: &[f64]| x.iter().zip(matvec(a, x)).map(|(p, q)| p * q).sum::<f64>();
    let energy: Vec<f64> =
        h.velocity.iter().zip(&h.displacement).map(|(v, u)| 0.5 * quad(m, v) + 0.5 * quad(k, u)).collect();
    let mut work = Vec::new();
    let mut dissipation = Vec::new();
    for n in 0..times.len() - 1 {
        let dt = times[n + 1] - times[n];
        work.push(
            (0..h.displacement[n].len())
                .map(|i| {
                    0.5 * (forces[n + 1][i] + forces[n][i])
                        * (h.displacement[n + 1][i] - h.displacement[n][i])
                })
                .sum::<f64>(),
        );
        let av: Vec<f64> = h.velocity[n + 1].iter().zip(&h.velocity[n]).map(|(a, b)| 0.5 * (a + b)).collect();
        dissipation.push(dt * quad(c, &av));
    }
    let balance: Vec<f64> =
        (0..work.len()).map(|n| energy[n + 1] - energy[n] - work[n] + dissipation[n]).collect();
    json!({"mechanical_energy_J": energy, "external_work_J": work, "damping_dissipation_J": dissipation, "balance_error_J": balance})
}


#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn checked_linear_history(
    mass: &DenseMatrix,
    damping: &DenseMatrix,
    stiffness: &DenseMatrix,
    forces: &[Vec<f64>],
    times: &[f64],
    u0: &[f64],
    v0: &[f64],
    history_byte_budget: i64,
    force_sampling: &str,
) -> Result<Value, CaeError> {
    if force_sampling != "nodal" && force_sampling != "interval_constant" {
        return contract("force_sampling must be nodal or interval_constant");
    }
    for (name, finite) in [
        ("mass", mass.data.iter().all(Scalar::is_finite)),
        ("damping", damping.data.iter().all(Scalar::is_finite)),
        ("stiffness", stiffness.data.iter().all(Scalar::is_finite)),
        ("forces", forces.iter().flatten().all(Scalar::is_finite)),
        ("times", times.iter().all(Scalar::is_finite)),
        ("u0", u0.iter().all(Scalar::is_finite)),
        ("v0", v0.iter().all(Scalar::is_finite)),
    ] {
        if !finite {
            return contract(format!("{name} must contain finite real numbers"));
        }
    }
    if u0.is_empty() || v0.len() != u0.len() {
        return contract("initial displacement and velocity require matching nonempty vectors");
    }
    let n = u0.len();
    let interval = force_sampling == "interval_constant";
    let expected_rows = times.len() - usize::from(interval);
    if times.len() < 2
        || times[0] != 0.0
        || times.windows(2).any(|w| w[1] - w[0] <= 0.0)
        || forces.len() != expected_rows
        || forces.iter().any(|f| f.len() != n)
    {
        return contract("forces must match strictly increasing times starting at zero and free DOFs");
    }
    let mut mats = Vec::new();
    for (name, a) in [("mass", mass), ("damping", damping), ("stiffness", stiffness)] {
        if a.nrows != n || a.ncols != n {
            return contract(format!("{name} must be a symmetric free-DOF matrix"));
        }
        let scale = a.data.iter().map(|v| v.abs()).fold(0.0, f64::max).max(f64::MIN_POSITIVE);
        let asym = (0..n)
            .flat_map(|i| (0..n).map(move |j| (i, j)))
            .map(|(i, j)| (a.get(i, j) - a.get(j, i)).abs())
            .fold(0.0, f64::max);
        if asym > 1e-12 * scale {
            return contract(format!("{name} must be a symmetric free-DOF matrix"));
        }
        let sym = DenseMatrix {
            nrows: n,
            ncols: n,
            data: (0..n * n).map(|p| 0.5 * (a.data[p] + a.data[(p % n) * n + p / n])).collect(),
        };
        let eig = linalg(implexity_linalg::dense::eigvalsh(&sym))?;
        let (lo, hi) = (eig[0], eig[eig.len() - 1]);
        if name == "mass" {
            if lo <= 0.0 || hi / lo > 1e12 {
                return contract("mass must be positive definite with condition number <= 1e12");
            }
        } else if lo < -1e-12 * eig.iter().map(|v| v.abs()).fold(0.0, f64::max).max(f64::MIN_POSITIVE) {
            return contract(format!("{name} must be positive semidefinite"));
        }
        mats.push(sym);
    }
    let (m, c, k) = (&mats[0], &mats[1], &mats[2]);
    let required = (if interval { 4 * times.len() - 2 } else { 3 * times.len() }) * n * 8;
    if history_byte_budget < i64::try_from(required).unwrap_or(i64::MAX) {
        return contract(format!("state history needs {required} bytes; this excludes matrix/AD workspace"));
    }
    let (history, ledger, residual, scale): (Value, Value, Vec<f64>, Vec<f64>) = if interval {
        let (h, step_ledger) = step_force_history(m, c, k, forces, times, u0, v0)?;

        let mut ledger = json!({});
        for key in ["mechanical_energy_J", "external_work_J", "damping_dissipation_J", "external_impulse_Ns"]
        {
            ledger[key] = step_ledger[key].clone();
        }
        ledger["balance_error_J"] = step_ledger["energy_balance_error_J"].clone();
        let (mut res, mut sc) = (Vec::new(), Vec::new());
        for (accs, offset) in [(&h.acceleration, 0usize), (&h.acceleration_end, 1usize)] {
            for (idx, a) in accs.iter().enumerate() {
                let (v, u) = (&h.velocity[idx + offset], &h.displacement[idx + offset]);
                let (t1, t2, t3) = (matvec(m, a), matvec(c, v), matvec(k, u));
                for i in 0..n {
                    res.push(t1[i] + t2[i] + t3[i] - forces[idx][i]);
                    sc.push((t1[i].abs() + t2[i].abs() + t3[i].abs() + forces[idx][i].abs()).max(1.0));
                }
            }
        }
        let history = json!({"displacement_m": h.displacement, "velocity_m_s": h.velocity,
            "interval_start_acceleration_m_s2": h.acceleration, "interval_end_acceleration_m_s2": h.acceleration_end});
        (history, ledger, res, sc)
    } else {
        let h = average_acceleration_history(m, c, k, forces, times, u0, v0)?;
        let ledger = energy_ledger(&h, m, c, k, forces, times);
        let (mut res, mut sc) = (Vec::new(), Vec::new());
        for idx in 0..times.len() {
            let (t1, t2, t3) = (
                matvec(m, &h.acceleration[idx]),
                matvec(c, &h.velocity[idx]),
                matvec(k, &h.displacement[idx]),
            );
            for i in 0..n {
                res.push(t1[i] + t2[i] + t3[i] - forces[idx][i]);
                sc.push((t1[i].abs() + t2[i].abs() + t3[i].abs() + forces[idx][i].abs()).max(1.0));
            }
        }
        let history = json!({"displacement_m": h.displacement, "velocity_m_s": h.velocity, "acceleration_m_s2": h.acceleration});
        (history, ledger, res, sc)
    };
    let all: Vec<f64> = crate::util::real_array(&history).map(|(_, v)| v).unwrap_or_default();
    let values = |key: &str| {
        ledger[key]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let (energy, work, diss, bal) = (
        values("mechanical_energy_J"),
        values("external_work_J"),
        values("damping_dissipation_J"),
        values("balance_error_J"),
    );
    let finite = history.as_object().is_some_and(|o| {
        o.values().all(|v| crate::util::real_array(v).is_some_and(|(_, d)| d.iter().all(Scalar::is_finite)))
    }) && energy.iter().chain(&work).chain(&diss).chain(&bal).all(Scalar::is_finite);
    let _ = all;
    if !finite {
        return contract("nonfinite structural history");
    }
    let error = residual.iter().zip(&scale).map(|(r, s)| r.abs() / s).fold(0.0, f64::max);
    let energy_error = (0..bal.len())
        .map(|i| {
            bal[i].abs() / (energy[i + 1].abs() + energy[i].abs() + work[i].abs() + diss[i].abs()).max(1.0)
        })
        .fold(0.0, f64::max);
    if error.max(energy_error) > 1e-10 {
        return contract("structural equilibrium or energy ledger failed");
    }
    Ok(json!({"history": history, "ledger": ledger, "maximum_scaled_equilibrium_error": error,
        "maximum_scaled_energy_error": energy_error, "history_bytes": required, "force_sampling": force_sampling,
        "scope": "Admitted fixed linear matrices; no spatial/modal/time-resolution or material calibration qualification."}))
}

