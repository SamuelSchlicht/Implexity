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



use crate::error::AdError;
use crate::scalar::Scalar;

fn check_square<S>(a: &[S], n: usize, what: &str) -> Result<(), AdError> {
    if a.len() == n * n {
        Ok(())
    } else {
        Err(AdError::Shape(format!("{what}: {} entries for a {n}×{n} matrix", a.len())))
    }
}



pub fn lu_in_place<S: Scalar>(a: &mut [S], n: usize) -> Result<(Vec<usize>, f64), AdError> {
    check_square(a, n, "lu")?;
    let mut piv = Vec::with_capacity(n);
    let mut sign = 1.0;
    for k in 0..n {
        let mut p = k;
        let mut best = a[k * n + k].value().abs();
        for i in k + 1..n {
            let v = a[i * n + k].value().abs();
            if v > best {
                best = v;
                p = i;
            }
        }
        piv.push(p);
        if p != k {
            for j in 0..n {
                a.swap(k * n + j, p * n + j);
            }
            sign = -sign;
        }
        let pivot = a[k * n + k];
        if pivot.value() == 0.0 {
            continue;
        }
        for i in k + 1..n {
            let l = a[i * n + k] / pivot;
            a[i * n + k] = l;
            for j in k + 1..n {
                let u = a[k * n + j];
                a[i * n + j] -= l * u;
            }
        }
    }
    Ok((piv, sign))
}



pub fn det<S: Scalar>(a: &[S], n: usize) -> Result<S, AdError> {
    match n {
        0 => return Ok(S::one()),
        1 => return a.first().copied().ok_or_else(|| AdError::Shape("det: empty".into())),
        2 => {
            check_square(a, n, "det")?;
            return Ok(a[0] * a[3] - a[1] * a[2]);
        }
        3 => {
            check_square(a, n, "det")?;
            return Ok(a[0] * (a[4] * a[8] - a[5] * a[7]) - a[1] * (a[3] * a[8] - a[5] * a[6])
                + a[2] * (a[3] * a[7] - a[4] * a[6]));
        }
        _ => {}
    }
    let mut lu = a.to_vec();
    let (_, sign) = lu_in_place(&mut lu, n)?;
    let mut d = S::from_f64(sign);
    for k in 0..n {
        d *= lu[k * n + k];
    }
    Ok(d)
}



pub fn solve<S: Scalar>(a: &[S], b: &[S], n: usize) -> Result<Vec<S>, AdError> {
    check_square(a, n, "solve")?;
    if n == 0 || !b.len().is_multiple_of(n) {
        if n == 0 && b.is_empty() {
            return Ok(Vec::new());
        }
        return Err(AdError::Shape(format!("solve: right-hand side of length {} for n = {n}", b.len())));
    }
    let m = b.len() / n;
    let mut lu = a.to_vec();
    let (piv, _) = lu_in_place(&mut lu, n)?;
    let mut x = b.to_vec();
    for (k, &p) in piv.iter().enumerate() {
        if p != k {
            for j in 0..m {
                x.swap(k * m + j, p * m + j);
            }
        }
    }
    for i in 0..n {
        for k in 0..i {
            let l = lu[i * n + k];
            for j in 0..m {
                let v = x[k * m + j];
                x[i * m + j] -= l * v;
            }
        }
    }
    for i in (0..n).rev() {
        for k in i + 1..n {
            let u = lu[i * n + k];
            for j in 0..m {
                let v = x[k * m + j];
                x[i * m + j] -= u * v;
            }
        }
        let d = lu[i * n + i];
        if d.value() == 0.0 {
            return Err(AdError::Singular(format!("solve: zero pivot in column {i}")));
        }
        for j in 0..m {
            x[i * m + j] /= d;
        }
    }
    Ok(x)
}



pub fn inv<S: Scalar>(a: &[S], n: usize) -> Result<Vec<S>, AdError> {
    let mut eye = vec![S::zero(); n * n];
    for i in 0..n {
        eye[i * n + i] = S::one();
    }
    solve(a, &eye, n)
}



pub fn matmul<S: Scalar>(a: &[S], b: &[S], n: usize, k: usize, m: usize) -> Result<Vec<S>, AdError> {
    if a.len() != n * k || b.len() != k * m {
        return Err(AdError::Shape(format!(
            "matmul: {}×? · {}×? with n={n}, k={k}, m={m}",
            a.len(),
            b.len()
        )));
    }
    let mut c = vec![S::zero(); n * m];
    for i in 0..n {
        for l in 0..k {
            let ail = a[i * k + l];
            for j in 0..m {
                c[i * m + j] += ail * b[l * m + j];
            }
        }
    }
    Ok(c)
}



pub fn transpose<S: Scalar>(a: &[S], n: usize, m: usize) -> Result<Vec<S>, AdError> {
    if a.len() != n * m {
        return Err(AdError::Shape(format!("transpose: {} entries for {n}×{m}", a.len())));
    }
    let mut t = vec![S::zero(); n * m];
    for i in 0..n {
        for j in 0..m {
            t[j * n + i] = a[i * m + j];
        }
    }
    Ok(t)
}



pub fn trace<S: Scalar>(a: &[S], n: usize) -> Result<S, AdError> {
    check_square(a, n, "trace")?;
    let mut t = S::zero();
    for i in 0..n {
        t += a[i * n + i];
    }
    Ok(t)
}

fn jacobi_eigh(a: &[f64], n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut m = a.to_vec();
    let mut v = vec![0.0; n * n];
    for i in 0..n {
        v[i * n + i] = 1.0;
    }
    for _sweep in 0..100 {
        let mut off = 0.0;
        let mut total = 0.0;
        for i in 0..n {
            for j in 0..n {
                let x = m[i * n + j] * m[i * n + j];
                total += x;
                if i != j {
                    off += x;
                }
            }
        }
        if off <= f64::EPSILON * f64::EPSILON * total || off == 0.0 {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = m[p * n + q];
                if apq == 0.0 {
                    continue;
                }
                let theta = (m[q * n + q] - m[p * n + p]) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let mkp = m[k * n + p];
                    let mkq = m[k * n + q];
                    m[k * n + p] = c * mkp - s * mkq;
                    m[k * n + q] = s * mkp + c * mkq;
                }
                for k in 0..n {
                    let mpk = m[p * n + k];
                    let mqk = m[q * n + k];
                    m[p * n + k] = c * mpk - s * mqk;
                    m[q * n + k] = s * mpk + c * mqk;
                }
                for k in 0..n {
                    let vkp = v[k * n + p];
                    let vkq = v[k * n + q];
                    v[k * n + p] = c * vkp - s * vkq;
                    v[k * n + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| m[i * n + i].total_cmp(&m[j * n + j]));
    let vals: Vec<f64> = order.iter().map(|&i| m[i * n + i]).collect();
    let mut vecs = vec![0.0; n * n];
    for (new, &old) in order.iter().enumerate() {
        for k in 0..n {
            vecs[k * n + new] = v[k * n + old];
        }
    }
    (vals, vecs)
}




pub fn eigvalsh<S: Scalar>(a: &[S], n: usize) -> Result<Vec<S>, AdError> {
    check_square(a, n, "eigvalsh")?;
    let mut sym = vec![S::zero(); n * n];
    for i in 0..n {
        for j in 0..n {
            sym[i * n + j] = (a[i * n + j] + a[j * n + i]) * 0.5;
        }
    }
    let values: Vec<f64> = sym.iter().map(Scalar::value).collect();
    let (lam, vecs) = jacobi_eigh(&values, n);
    let col = |k: usize, i: usize| vecs[k * n + i];
    let mut out = Vec::with_capacity(n);
    let mut grad = vec![0.0; n * n];
    let mut proj = vec![S::zero(); n];
    for i in 0..n {
        for j in 0..n {
            for k in 0..n {
                grad[j * n + k] = col(j, i) * col(k, i);
            }
        }
        let first = S::lift(lam[i], &sym, &grad, &[]);
        let mut diag = vec![0.0; n * n];
        for m in 0..n {
            let mut g = vec![0.0; n * n];
            for j in 0..n {
                for k in 0..n {
                    g[j * n + k] = col(j, i) * col(k, m);
                }
            }
            proj[m] = S::lift(0.0, &sym, &g, &[]);
            if m != i {
                diag[m * n + m] = 2.0 / (lam[i] - lam[m]);
            }
        }
        let correction = S::lift(0.0, &proj, &vec![0.0; n], &diag);
        out.push(first + correction);
    }
    Ok(out)
}

