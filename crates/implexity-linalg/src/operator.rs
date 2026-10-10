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


use crate::error::LinalgError;
use crate::lu::SparseLu;
use crate::sparse::{CscMatrix, CsrMatrix};


pub trait LinearOperator {
    fn n(&self) -> usize;



    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError>;



    fn matvec(&self, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        let mut y = vec![0.0; self.n()];
        self.apply(x, &mut y)?;
        Ok(y)
    }



    fn apply_block(&self, xs: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, LinalgError> {
        xs.iter().map(|x| self.matvec(x)).collect()
    }
}

impl<T: LinearOperator + ?Sized> LinearOperator for &T {
    fn n(&self) -> usize {
        (**self).n()
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        (**self).apply(x, y)
    }
    fn apply_block(&self, xs: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, LinalgError> {
        (**self).apply_block(xs)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Identity(pub usize);

impl LinearOperator for Identity {
    fn n(&self) -> usize {
        self.0
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        check(self.0, x, y)?;
        y.copy_from_slice(x);
        Ok(())
    }
}

pub struct FnOperator<F> {
    n: usize,
    f: F,
}

impl<F> FnOperator<F>
where
    F: Fn(&[f64], &mut [f64]) -> Result<(), LinalgError>,
{
    pub const fn new(n: usize, f: F) -> Self {
        Self { n, f }
    }
}

impl<F> LinearOperator for FnOperator<F>
where
    F: Fn(&[f64], &mut [f64]) -> Result<(), LinalgError>,
{
    fn n(&self) -> usize {
        self.n
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        check(self.n, x, y)?;
        (self.f)(x, y)
    }
}

fn check(n: usize, x: &[f64], y: &[f64]) -> Result<(), LinalgError> {
    if x.len() == n && y.len() == n {
        Ok(())
    } else {
        Err(LinalgError::Shape(format!("operator of order {n} applied to {} → {}", x.len(), y.len())))
    }
}

impl LinearOperator for CsrMatrix {
    fn n(&self) -> usize {
        self.nrows()
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        if self.nrows() != self.ncols() {
            return Err(LinalgError::Shape("operator must be square".into()));
        }
        self.matvec_into(x, y)
    }
}

impl LinearOperator for CscMatrix {
    fn n(&self) -> usize {
        self.nrows()
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        if self.nrows() != self.ncols() {
            return Err(LinalgError::Shape("operator must be square".into()));
        }
        check(self.nrows(), x, y)?;
        y.copy_from_slice(&self.matvec(x)?);
        Ok(())
    }
}

impl LinearOperator for SparseLu {
    fn n(&self) -> usize {
        SparseLu::n(self)
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        check(SparseLu::n(self), x, y)?;
        y.copy_from_slice(x);
        self.solve_in_place(y)
    }
    fn apply_block(&self, xs: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, LinalgError> {
        solve_block(self, xs, false)
    }
}



pub fn solve_block(lu: &SparseLu, xs: &[Vec<f64>], transpose: bool) -> Result<Vec<Vec<f64>>, LinalgError> {
    let n = SparseLu::n(lu);
    if xs.iter().any(|x| x.len() != n) {
        return Err(LinalgError::Shape(format!(
            "operator of order {n} applied to a block of another length"
        )));
    }
    if n == 0 {
        return Ok(xs.to_vec());
    }
    let mut block: Vec<f64> = xs.iter().flatten().copied().collect();
    lu.solve_many_in_place(&mut block, xs.len(), transpose)?;
    Ok(block.chunks_exact(n).map(<[f64]>::to_vec).collect())
}

#[inline]
#[must_use]
pub fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[must_use]
pub fn nrm2(x: &[f64]) -> f64 {
    let scale = x.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    if scale == 0.0 || !scale.is_finite() {
        return if scale.is_nan() || x.iter().any(|v| v.is_nan()) { f64::NAN } else { scale };
    }
    if (1e-150..1e150).contains(&scale) {
        return x.iter().map(|v| v * v).sum::<f64>().sqrt();
    }
    let s: f64 = x.iter().map(|v| (v / scale) * (v / scale)).sum();
    scale * s.sqrt()
}

#[inline]
pub fn axpy(a: f64, x: &[f64], y: &mut [f64]) {
    for (yi, xi) in y.iter_mut().zip(x) {
        *yi += a * xi;
    }
}

#[inline]
pub fn scal(a: f64, x: &mut [f64]) {
    for v in x {
        *v *= a;
    }
}
