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


pub const Q: usize = 19;

pub const C: [[i32; 3]; Q] = [
    [0, 0, 0],
    [-1, -1, 0],
    [-1, 0, -1],
    [-1, 0, 0],
    [-1, 0, 1],
    [-1, 1, 0],
    [0, -1, -1],
    [0, -1, 0],
    [0, -1, 1],
    [0, 0, -1],
    [0, 0, 1],
    [0, 1, -1],
    [0, 1, 0],
    [0, 1, 1],
    [1, -1, 0],
    [1, 0, -1],
    [1, 0, 0],
    [1, 0, 1],
    [1, 1, 0],
];

const W0: f64 = 1.0 / 3.0;
const W1: f64 = 1.0 / 18.0;
const W2: f64 = 1.0 / 36.0;

pub const W: [f64; Q] = [W0, W2, W2, W1, W2, W2, W2, W1, W2, W1, W1, W2, W1, W2, W2, W2, W1, W2, W2];

pub const OPPOSITE: [usize; Q] = [0, 18, 17, 16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1];

pub const POSITIVE: [usize; 9] = [10, 11, 12, 13, 14, 15, 16, 17, 18];

#[inline]
#[must_use]
pub fn c(q: usize, d: usize) -> f64 {
    f64::from(C[q][d])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub shape: [usize; 3],
}

impl Grid {
    #[must_use]
    pub const fn new(shape: [usize; 3]) -> Self {
        Self { shape }
    }

    #[must_use]
    pub const fn cells(&self) -> usize {
        self.shape[0] * self.shape[1] * self.shape[2]
    }

    #[inline]
    #[must_use]
    pub const fn index(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.shape[1] + j) * self.shape[2] + k
    }

    #[inline]
    #[must_use]
    pub const fn coords(&self, cell: usize) -> [usize; 3] {
        let k = cell % self.shape[2];
        let ij = cell / self.shape[2];
        [ij / self.shape[1], ij % self.shape[1], k]
    }

    #[inline]
    #[must_use]
    pub fn wrap(&self, cell: usize, offset: [i64; 3]) -> usize {
        let x = self.coords(cell);
        let mut out = [0usize; 3];
        for a in 0..3 {
            #[allow(clippy::cast_possible_wrap)]
            let n = self.shape[a] as i64;
            #[allow(clippy::cast_possible_wrap)]
            let v = (x[a] as i64 + offset[a]).rem_euclid(n);
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            {
                out[a] = v as usize;
            }
        }
        self.index(out[0], out[1], out[2])
    }

    #[inline]
    #[must_use]
    pub fn inside(&self, cell: usize, offset: [i64; 3], periodic: [bool; 3]) -> bool {
        let x = self.coords(cell);
        (0..3).all(|a| {
            #[allow(clippy::cast_possible_wrap)]
            let v = x[a] as i64 + offset[a];
            #[allow(clippy::cast_possible_wrap)]
            let n = self.shape[a] as i64;
            periodic[a] || (0..n).contains(&v)
        })
    }

    #[must_use]
    pub fn roll<T: Copy>(&self, values: &[T], shift: [i64; 3]) -> Vec<T> {
        (0..self.cells()).map(|x| values[self.wrap(x, [-shift[0], -shift[1], -shift[2]])]).collect()
    }
}


