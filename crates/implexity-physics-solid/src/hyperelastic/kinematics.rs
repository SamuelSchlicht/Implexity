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



use implexity_ad::Scalar;
use implexity_core::CaeError;

use crate::util::contract;

pub type Mat3<S> = [[S; 3]; 3];

#[must_use]
pub fn identity<S: Scalar>() -> Mat3<S> {
    std::array::from_fn(|i| std::array::from_fn(|j| if i == j { S::one() } else { S::zero() }))
}

pub fn matmul<S: Scalar>(a: &Mat3<S>, b: &Mat3<S>) -> Mat3<S> {
    std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            let mut acc = S::zero();
            for k in 0..3 {
                acc += a[i][k] * b[k][j];
            }
            acc
        })
    })
}

pub fn transpose<S: Scalar>(a: &Mat3<S>) -> Mat3<S> {
    std::array::from_fn(|i| std::array::from_fn(|j| a[j][i]))
}

pub fn det<S: Scalar>(m: &Mat3<S>) -> S {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

pub fn inverse_transpose<S: Scalar>(m: &Mat3<S>) -> Mat3<S> {
    let d = det(m);
    let c = |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    [
        [c(1, 1, 2, 2) / d, -c(1, 0, 2, 2) / d, c(1, 0, 2, 1) / d],
        [-c(0, 1, 2, 2) / d, c(0, 0, 2, 2) / d, -c(0, 0, 2, 1) / d],
        [c(0, 1, 1, 2) / d, -c(0, 0, 1, 2) / d, c(0, 0, 1, 1) / d],
    ]
}

pub fn ddot<S: Scalar>(a: &Mat3<S>, b: &Mat3<S>) -> S {
    let mut acc = S::zero();
    for i in 0..3 {
        for j in 0..3 {
            acc += a[i][j] * b[i][j];
        }
    }
    acc
}

pub fn green_strain<S: Scalar>(f: &Mat3<S>) -> Mat3<S> {
    let c = matmul(&transpose(f), f);
    std::array::from_fn(|i| std::array::from_fn(|j| (c[i][j] - if i == j { 1.0 } else { 0.0 }) * 0.5))
}

#[derive(Debug, Clone, PartialEq)]
pub struct TetMesh {
    pub points: Vec<[f64; 3]>,
    pub elements: Vec<[usize; 4]>,
    pub gradients: Vec<[[f64; 3]; 4]>,
    pub volumes: Vec<f64>,
}

fn inverse4(m: [[f64; 4]; 4]) -> Option<[[f64; 4]; 4]> {
    let mut a = m;
    let mut inv = [[0.0; 4]; 4];
    for (i, row) in inv.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for col in 0..4 {
        let pivot = (col..4).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col] == 0.0 {
            return None;
        }
        a.swap(col, pivot);
        inv.swap(col, pivot);
        let p = a[col][col];
        for j in 0..4 {
            a[col][j] /= p;
            inv[col][j] /= p;
        }
        for r in 0..4 {
            if r != col {
                let f = a[r][col];
                for j in 0..4 {
                    a[r][j] -= f * a[col][j];
                    inv[r][j] -= f * inv[col][j];
                }
            }
        }
    }
    Some(inv)
}

impl TetMesh {

    pub fn new(points: Vec<[f64; 3]>, elements: Vec<[usize; 4]>) -> Result<Self, CaeError> {
        let mut gradients = Vec::with_capacity(elements.len());
        let mut volumes = Vec::with_capacity(elements.len());
        for t in &elements {
            if t.iter().any(|n| *n >= points.len()) {
                return contract("invalid connectivity or unused mesh nodes");
            }
            let affine: [[f64; 4]; 4] =
                std::array::from_fn(|i| [1.0, points[t[i]][0], points[t[i]][1], points[t[i]][2]]);
            let Some(inv) = inverse4(affine) else {
                return contract("inverted or numerically degenerate tetrahedron");
            };
            gradients.push(std::array::from_fn(|a| std::array::from_fn(|i| inv[i + 1][a])));
            let edges: Mat3<f64> =
                std::array::from_fn(|r| std::array::from_fn(|c| points[t[r + 1]][c] - points[t[0]][c]));
            volumes.push(det(&edges) / 6.0);
        }
        Ok(Self { points, elements, gradients, volumes })
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.points.len()
    }

    pub fn deformation_gradient<S: Scalar>(&self, e: usize, u: &[[S; 3]; 4]) -> Mat3<S> {
        let g = &self.gradients[e];
        std::array::from_fn(|i| {
            std::array::from_fn(|j| {
                let mut acc = if i == j { S::one() } else { S::zero() };
                for a in 0..4 {
                    acc += u[a][i] * g[a][j];
                }
                acc
            })
        })
    }

    pub fn nodal_forces<S: Scalar>(&self, e: usize, piola: &Mat3<S>) -> [[S; 3]; 4] {
        let g = &self.gradients[e];
        let v = self.volumes[e];
        std::array::from_fn(|a| {
            std::array::from_fn(|i| {
                let mut acc = S::zero();
                for j in 0..3 {
                    acc += piola[i][j] * g[a][j];
                }
                acc * v
            })
        })
    }

    pub fn hydraulic_local<S: Scalar>(
        &self,
        e: usize,
        biot_modulus: S,
        mobility: S,
    ) -> ([[S; 4]; 4], [[S; 4]; 4]) {
        let v = self.volumes[e];
        let g = &self.gradients[e];
        let s = std::array::from_fn(|a| {
            std::array::from_fn(|b| S::from_f64(v) / biot_modulus * (if a == b { 2.0 } else { 1.0 } / 20.0))
        });
        let k = std::array::from_fn(|a| {
            std::array::from_fn(|b| {
                mobility * (v * (g[a][0] * g[b][0] + g[a][1] * g[b][1] + g[a][2] * g[b][2]))
            })
        });
        (s, k)
    }
}

#[derive(Debug, Clone)]
pub struct GreenMaxwell<S> {
    pub branch_strain: Vec<Mat3<S>>,
    pub stored: S,
    pub dissipated: S,
    pub potential: S,
    pub first_piola: Mat3<S>,
}

pub fn green_maxwell_step<S: Scalar>(
    f: &Mat3<S>,
    previous: &[Mat3<S>],
    moduli: &[S],
    times: &[S],
    dt: S,
) -> GreenMaxwell<S> {
    let strain = green_strain(f);
    let mut out = GreenMaxwell {
        branch_strain: Vec::with_capacity(previous.len()),
        stored: S::zero(),
        dissipated: S::zero(),
        potential: S::zero(),
        first_piola: [[S::zero(); 3]; 3],
    };
    let mut second = [[S::zero(); 3]; 3];
    for (b, prev) in previous.iter().enumerate() {
        let modulus = moduli[b];
        let ratio = dt / times[b];
        let state: Mat3<S> = std::array::from_fn(|i| {
            std::array::from_fn(|j| (prev[i][j] + ratio * strain[i][j]) / (ratio + 1.0))
        });
        let elastic: Mat3<S> = std::array::from_fn(|i| std::array::from_fn(|j| strain[i][j] - state[i][j]));
        let diff: Mat3<S> = std::array::from_fn(|i| std::array::from_fn(|j| strain[i][j] - prev[i][j]));
        let e2 = ddot(&elastic, &elastic);
        out.stored += modulus * e2 * 0.5;
        out.dissipated += modulus * ratio * e2;
        out.potential += modulus / (ratio + 1.0) * ddot(&diff, &diff) * 0.5;
        for i in 0..3 {
            for j in 0..3 {
                second[i][j] += modulus * elastic[i][j];
            }
        }
        out.branch_strain.push(state);
    }
    out.first_piola = matmul(f, &second);
    out
}

pub fn neo_hookean_energy<S: Scalar>(f: &Mat3<S>, mu: S, lam: S) -> S {
    let j = det(f);
    let lj = j.ln();
    mu * (ddot(f, f) - 3.0) * 0.5 - mu * lj + lam * lj * lj * 0.5
}

pub fn neo_hookean_piola<S: Scalar>(f: &Mat3<S>, mu: S, lam: S) -> Mat3<S> {
    let inv_t = inverse_transpose(f);
    let lj = det(f).ln();
    std::array::from_fn(|i| std::array::from_fn(|j| mu * (f[i][j] - inv_t[i][j]) + lam * lj * inv_t[i][j]))
}

#[must_use]
pub fn neo_hookean_cauchy(f: &Mat3<f64>, mu: f64, lam: f64) -> Mat3<f64> {
    let j = det(f);
    let b = matmul(f, &transpose(f));
    std::array::from_fn(|r| {
        std::array::from_fn(|c| {
            mu / j * (b[r][c] - if r == c { 1.0 } else { 0.0 }) + if r == c { lam * j.ln() / j } else { 0.0 }
        })
    })
}

pub fn maxwell_potential_piola<S: Scalar>(
    f: &Mat3<S>,
    previous: &[Mat3<S>],
    moduli: &[S],
    times: &[S],
    dt: S,
) -> Mat3<S> {
    let strain = green_strain(f);
    let mut s = [[S::zero(); 3]; 3];
    for (b, prev) in previous.iter().enumerate() {
        let w = moduli[b] / (dt / times[b] + 1.0);
        for i in 0..3 {
            for j in 0..3 {
                s[i][j] += w * (strain[i][j] - prev[i][j]);
            }
        }
    }
    matmul(f, &s)
}
