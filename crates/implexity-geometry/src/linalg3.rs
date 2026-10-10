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


