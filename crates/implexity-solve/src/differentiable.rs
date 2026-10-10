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



use implexity_ad::{Dual, Scalar};
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;

pub const WIDTH: usize = 8;

pub trait DifferentiableResidual: Send + Sync {
    fn residual<S: Scalar>(&self, u: &[S], x: &[S]) -> Vec<S>;
}

pub trait DifferentiableResponse: Send + Sync {
    fn value<S: Scalar>(&self, u: &[S], x: &[S]) -> S;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Argument {
    State,
    Design,
}

fn lift(v: &[f64]) -> Vec<Dual<WIDTH>> {
    v.iter().map(|&x| Dual::constant(x)).collect()
}

fn seed(values: &mut [Dual<WIDTH>], start: usize, end: usize) {
    for (j, s) in values.iter_mut().enumerate() {
        s.eps = [0.0; WIDTH];
        if (start..end).contains(&j) {
            s.eps[j - start] = 1.0;
        }
    }
}



pub fn evaluate<R: DifferentiableResidual>(r: &R, u: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
    let out = r.residual::<f64>(u, x);
    if out.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::convergence("residual contains non-finite values"));
    }
    Ok(out)
}

#[must_use]
pub fn jacobian<R: DifferentiableResidual>(r: &R, u: &[f64], x: &[f64], wrt: Argument) -> DenseMatrix {
    let (mut du, mut dx) = (lift(u), lift(x));
    let cols = if wrt == Argument::State { u.len() } else { x.len() };
    let mut out: Option<DenseMatrix> = None;
    for pass in 0..cols.div_ceil(WIDTH).max(1) {
        let start = pass * WIDTH;
        let end = (start + WIDTH).min(cols);
        match wrt {
            Argument::State => seed(&mut du, start, end),
            Argument::Design => seed(&mut dx, start, end),
        }
        let y = r.residual(&du, &dx);
        let m = out.get_or_insert_with(|| DenseMatrix::zeros(y.len(), cols));
        for (i, yi) in y.iter().enumerate() {
            for j in start..end {
                m.data[i * cols + j] = yi.eps[j - start];
            }
        }
    }
    out.unwrap_or_else(|| DenseMatrix::zeros(0, cols))
}

#[must_use]
pub fn jvp_state<R: DifferentiableResidual>(r: &R, u: &[f64], x: &[f64], v: &[f64]) -> CaeResult<Vec<f64>> {
    if v.len() != u.len() {
        return Err(CaeError::contract("state direction length differs from state length"));
    }
    let du: Vec<Dual<1>> = u.iter().zip(v).map(|(&a, &b)| Dual::new(a, [b])).collect();
    let dx: Vec<Dual<1>> = x.iter().map(|&a| Dual::constant(a)).collect();
    Ok(r.residual(&du, &dx).iter().map(|y| y.eps[0]).collect())
}

#[must_use]
pub fn vjp<R: DifferentiableResidual>(r: &R, u: &[f64], x: &[f64], w: &[f64], wrt: Argument) -> CaeResult<Vec<f64>> {
    let (mut du, mut dx) = (lift(u), lift(x));
    let cols = if wrt == Argument::State { u.len() } else { x.len() };
    let mut out = vec![0.0; cols];
    for pass in 0..cols.div_ceil(WIDTH).max(1) {
        let start = pass * WIDTH;
        let end = (start + WIDTH).min(cols);
        match wrt {
            Argument::State => seed(&mut du, start, end),
            Argument::Design => seed(&mut dx, start, end),
        }
        let y = r.residual(&du, &dx);
        if y.len() != w.len() {
            return Err(CaeError::contract("residual cotangent length differs from residual length"));
        }
        for (k, o) in out[start..end].iter_mut().enumerate() {
            *o = y.iter().zip(w).map(|(yi, wi)| wi * yi.eps[k]).sum();
        }
    }
    Ok(out)
}

#[must_use]
pub fn gradient<J: DifferentiableResponse>(j: &J, u: &[f64], x: &[f64], wrt: Argument) -> Vec<f64> {
    let (mut du, mut dx) = (lift(u), lift(x));
    let cols = if wrt == Argument::State { u.len() } else { x.len() };
    let mut out = vec![0.0; cols];
    for pass in 0..cols.div_ceil(WIDTH) {
        let start = pass * WIDTH;
        let end = (start + WIDTH).min(cols);
        match wrt {
            Argument::State => seed(&mut du, start, end),
            Argument::Design => seed(&mut dx, start, end),
        }
        let y = j.value(&du, &dx);
        out[start..end].copy_from_slice(&y.eps[..end - start]);
    }
    out
}

#[must_use]
pub fn response_value<J: DifferentiableResponse>(j: &J, u: &[f64], x: &[f64]) -> f64 {
    j.value::<f64>(u, x)
}

