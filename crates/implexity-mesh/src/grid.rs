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


use crate::MeshError;

#[derive(Clone, Copy, Debug)]
pub struct Field3<'a> {
    pub shape: [usize; 3],
    pub data: &'a [f64],
}

impl<'a> Field3<'a> {


    pub fn new(shape: [usize; 3], data: &'a [f64]) -> Result<Self, MeshError> {
        let n = shape[0].checked_mul(shape[1]).and_then(|v| v.checked_mul(shape[2]));
        if n != Some(data.len()) {
            return Err(MeshError::invalid("field samples do not match the grid shape"));
        }
        Ok(Self { shape, data })
    }

    #[must_use]
    #[inline]
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.shape[1] + j) * self.shape[2] + k
    }

    #[must_use]
    #[inline]
    pub fn at(&self, i: usize, j: usize, k: usize) -> f64 {
        self.data[self.index(i, j, k)]
    }

    #[must_use]
    pub fn min_max(&self) -> (f64, f64) {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for &v in self.data {
            if v.is_nan() {
                return (f64::NAN, f64::NAN);
            }
            lo = lo.min(v);
            hi = hi.max(v);
        }
        (lo, hi)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grid3 {
    pub shape: [usize; 3],
    pub data: Vec<f64>,
}

impl Grid3 {
    #[must_use]
    pub fn filled(shape: [usize; 3], value: f64) -> Self {
        Self { shape, data: vec![value; shape[0] * shape[1] * shape[2]] }
    }



    pub fn from_vec(shape: [usize; 3], data: Vec<f64>) -> Result<Self, MeshError> {
        Field3::new(shape, &data)?;
        Ok(Self { shape, data })
    }

    #[must_use]
    pub fn view(&self) -> Field3<'_> {
        Field3 { shape: self.shape, data: &self.data }
    }

    #[must_use]
    #[inline]
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.shape[1] + j) * self.shape[2] + k
    }

    #[must_use]
    #[inline]
    pub fn at(&self, i: usize, j: usize, k: usize) -> f64 {
        self.data[self.index(i, j, k)]
    }

    #[inline]
    pub fn at_mut(&mut self, i: usize, j: usize, k: usize) -> &mut f64 {
        let idx = self.index(i, j, k);
        &mut self.data[idx]
    }
}
