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


use rayon::prelude::*;

use crate::dual::Dual;
use crate::error::AdError;
use crate::hyperdual::HyperDual;

#[derive(Clone, Debug, PartialEq)]
pub struct Jacobian {
    pub value: Vec<f64>,
    pub matrix: Vec<f64>,
    pub rows: usize,
    pub cols: usize,
}

impl Jacobian {


    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> f64 {
        assert!(i < self.rows && j < self.cols, "Jacobian index out of range");
        self.matrix[i * self.cols + j]
    }
}




pub fn jacobian<const N: usize, F>(f: F, x: &[f64]) -> Result<Jacobian, AdError>
where
    F: Fn(&[Dual<N>]) -> Vec<Dual<N>>,
{
    const { assert!(N > 0, "Dual width N must be positive") };
    let n = x.len();
    let mut seeded: Vec<Dual<N>> = x.iter().map(|&v| Dual::constant(v)).collect();
    let passes = n.div_ceil(N).max(1);
    let mut value = Vec::new();
    let mut matrix = Vec::new();
    let mut rows = None;
    for pass in 0..passes {
        let start = pass * N;
        let end = (start + N).min(n);
        for (j, s) in seeded.iter_mut().enumerate() {
            s.eps = [0.0; N];
            if (start..end).contains(&j) {
                s.eps[j - start] = 1.0;
            }
        }
        let out = f(&seeded);
        let m = *rows.get_or_insert(out.len());
        if out.len() != m {
            return Err(AdError::Shape(format!(
                "jacobian: output length changed from {m} to {} between passes",
                out.len()
            )));
        }
        if pass == 0 {
            value = out.iter().map(|d| d.re).collect();
            matrix = vec![0.0; m * n];
        }
        for (i, y) in out.iter().enumerate() {
            for j in start..end {
                matrix[i * n + j] = y.eps[j - start];
            }
        }
    }
    let rows = rows.unwrap_or(0);
    Ok(Jacobian { value, matrix, rows, cols: n })
}



pub fn gradient<const N: usize, F>(f: F, x: &[f64]) -> Result<(f64, Vec<f64>), AdError>
where
    F: Fn(&[Dual<N>]) -> Dual<N>,
{
    let jac = jacobian::<N, _>(|xs| vec![f(xs)], x)?;
    Ok((jac.value[0], jac.matrix))
}



pub fn jvp<F>(f: F, x: &[f64], v: &[f64]) -> Result<(Vec<f64>, Vec<f64>), AdError>
where
    F: Fn(&[Dual<1>]) -> Vec<Dual<1>>,
{
    if v.len() != x.len() {
        return Err(AdError::Shape(format!("jvp: tangent length {} != input length {}", v.len(), x.len())));
    }
    let xs: Vec<Dual<1>> = x.iter().zip(v).map(|(&a, &b)| Dual::new(a, [b])).collect();
    let out = f(&xs);
    Ok((out.iter().map(|d| d.re).collect(), out.iter().map(|d| d.eps[0]).collect()))
}


#[must_use]
pub fn hessian<F>(f: F, x: &[f64]) -> (f64, Vec<f64>, Vec<f64>)
where
    F: Fn(&[HyperDual]) -> HyperDual,
{
    let n = x.len();
    let mut xs: Vec<HyperDual> = x.iter().map(|&v| HyperDual::constant(v)).collect();
    let mut grad = vec![0.0; n];
    let mut hess = vec![0.0; n * n];
    let mut value = f(&xs).re;
    for i in 0..n {
        for j in i..n {
            for (k, s) in xs.iter_mut().enumerate() {
                s.e1 = if k == i { 1.0 } else { 0.0 };
                s.e2 = if k == j { 1.0 } else { 0.0 };
                s.e12 = 0.0;
            }
            let y = f(&xs);
            value = y.re;
            if j == i {
                grad[i] = y.e1;
            }
            hess[i * n + j] = y.e12;
            hess[j * n + i] = y.e12;
        }
    }
    (value, grad, hess)
}



pub fn hessian_vector_product<F>(f: F, x: &[f64], v: &[f64]) -> Result<Vec<f64>, AdError>
where
    F: Fn(&[HyperDual]) -> HyperDual,
{
    let n = x.len();
    if v.len() != n {
        return Err(AdError::Shape(format!("hvp: direction length {} != input length {n}", v.len())));
    }
    let mut xs: Vec<HyperDual> = x.iter().zip(v).map(|(&a, &d)| HyperDual::new(a, d, 0.0, 0.0)).collect();
    let mut out = vec![0.0; n];
    for (j, o) in out.iter_mut().enumerate() {
        for (k, s) in xs.iter_mut().enumerate() {
            s.e2 = if k == j { 1.0 } else { 0.0 };
        }
        *o = f(&xs).e12;
    }
    Ok(out)
}




pub fn element_jacobians<const N: usize, K>(
    n_elements: usize,
    n_in: usize,
    n_out: usize,
    inputs: &[f64],
    kernel: K,
) -> Result<(Vec<f64>, Vec<f64>), AdError>
where
    K: Fn(usize, &[Dual<N>], &mut [Dual<N>]) + Sync,
{
    if n_in > N {
        return Err(AdError::Shape(format!("element_jacobians: {n_in} inputs exceed dual width {N}")));
    }
    if inputs.len() != n_elements * n_in {
        return Err(AdError::Shape(format!(
            "element_jacobians: {} input values for {n_elements} elements of {n_in}",
            inputs.len()
        )));
    }
    let mut values = vec![0.0; n_elements * n_out];
    let mut jacs = vec![0.0; n_elements * n_out * n_in];
    if n_out == 0 || n_elements == 0 {
        return Ok((values, jacs));
    }
    if n_in == 0 {
        values.par_chunks_mut(n_out).enumerate().for_each(|(e, val)| {
            let mut ys = vec![Dual::<N>::default(); n_out];
            kernel(e, &[], &mut ys);
            for (v, y) in val.iter_mut().zip(&ys) {
                *v = y.re;
            }
        });
        return Ok((values, jacs));
    }
    values.par_chunks_mut(n_out).zip(jacs.par_chunks_mut(n_out * n_in)).enumerate().for_each_init(
        || (Vec::with_capacity(n_in), vec![Dual::<N>::default(); n_out]),
        |(xs, ys), (e, (val, jac))| {
            xs.clear();
            xs.extend(
                inputs[e * n_in..(e + 1) * n_in].iter().enumerate().map(|(k, &v)| Dual::variable(v, k)),
            );
            ys.fill(Dual::default());
            kernel(e, xs, ys);
            for (i, y) in ys.iter().enumerate() {
                val[i] = y.re;
                jac[i * n_in..(i + 1) * n_in].copy_from_slice(&y.eps[..n_in]);
            }
        },
    );
    Ok((values, jacs))
}
