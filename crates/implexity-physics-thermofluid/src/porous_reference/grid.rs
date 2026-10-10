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


#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub nelx: usize,
    pub nely: usize,
    pub nelz: usize,
    pub h: f64,
    pub origin: [f64; 3],
}

impl Grid {
    #[must_use]
    pub fn new(nelx: usize, nely: usize, nelz: usize, h: f64) -> Self {
        Self { nelx, nely, nelz, h, origin: [0.0; 3] }
    }

    #[must_use]
    pub fn shape(&self) -> [usize; 3] {
        [self.nelx, self.nely, self.nelz]
    }

    #[must_use]
    pub fn node_shape(&self) -> [usize; 3] {
        [self.nelx + 1, self.nely + 1, self.nelz + 1]
    }

    #[must_use]
    pub fn nel(&self) -> usize {
        self.nelx * self.nely * self.nelz
    }

    #[must_use]
    pub fn nnode(&self) -> usize {
        (self.nelx + 1) * (self.nely + 1) * (self.nelz + 1)
    }

    #[must_use]
    pub fn extent(&self) -> [f64; 3] {
        [self.nelx as f64 * self.h, self.nely as f64 * self.h, self.nelz as f64 * self.h]
    }

    #[must_use]
    pub fn counts(&self) -> [usize; 3] {
        self.shape()
    }

    #[must_use]
    pub fn element_axis(&self, axis: usize) -> Vec<f64> {
        let n = self.counts()[axis];
        (0..n).map(|i| self.origin[axis] + (i as f64 + 0.5) * self.h).collect()
    }

    #[must_use]
    pub fn node_axis(&self, axis: usize) -> Vec<f64> {
        let n = self.counts()[axis];
        (0..=n).map(|i| self.origin[axis] + i as f64 * self.h).collect()
    }

    #[must_use]
    pub fn cell_volume(&self) -> f64 {
        self.h.powi(3)
    }

    #[must_use]
    pub fn face_area(&self) -> f64 {
        self.h.powi(2)
    }

    #[must_use]
    pub fn describe(&self) -> String {
        let e = self.extent();
        format!(
            "{}x{}x{} elements, h = {:.4} mm, domain {:.2} x {:.2} x {:.2} mm",
            self.nelx,
            self.nely,
            self.nelz,
            1e3 * self.h,
            1e3 * e[0],
            1e3 * e[1],
            1e3 * e[2]
        )
    }
}

#[must_use]
pub fn flat(s: [usize; 3], i: usize, j: usize, k: usize) -> usize {
    (i * s[1] + j) * s[2] + k
}

pub fn for_box(lo: [usize; 3], hi: [usize; 3], mut f: impl FnMut(usize, usize, usize)) {
    for i in lo[0]..hi[0] {
        for j in lo[1]..hi[1] {
            for k in lo[2]..hi[2] {
                f(i, j, k);
            }
        }
    }
}

#[must_use]
pub fn node_index(ix: usize, iy: usize, iz: usize, nely: usize, nelz: usize) -> usize {
    (ix * (nely + 1) + iy) * (nelz + 1) + iz
}

#[must_use]
pub fn element_index(ex: usize, ey: usize, ez: usize, nely: usize, nelz: usize) -> usize {
    (ex * nely + ey) * nelz + ez
}
