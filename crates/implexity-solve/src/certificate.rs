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





pub fn stable_l2(values: &[f64]) -> Result<f64, String> {
    if values.iter().any(|v| !v.is_finite()) {
        return Err("residual norm requires finite values".into());
    }
    let norm = values.iter().fold(0.0_f64, |acc, &v| acc.hypot(v));
    if !norm.is_finite() {
        return Err("residual L2 norm exceeds finite representable range".into());
    }
    Ok(norm)
}



pub fn stable_l2_columns(values: &[f64], rows: usize, cols: usize) -> Result<Vec<f64>, String> {
    if values.iter().any(|v| !v.is_finite()) {
        return Err("residual norm requires finite values".into());
    }
    let mut out = vec![0.0_f64; cols];
    for r in 0..rows {
        for (c, acc) in out.iter_mut().enumerate() {
            *acc = acc.hypot(values[r * cols + c]);
        }
    }
    if out.iter().any(|v| !v.is_finite()) {
        return Err("residual L2 norm exceeds finite representable range".into());
    }
    Ok(out)
}



pub fn residual_certificate(error: &[f64], rhs: &[f64]) -> Result<(f64, f64), String> {
    residual_certificate_with_floor(error, rhs, 1.0)
}



pub fn residual_certificate_with_floor(error: &[f64], rhs: &[f64], floor: f64) -> Result<(f64, f64), String> {
    if error.len() != rhs.len() {
        return Err("residual and RHS shapes differ".into());
    }
    if !floor.is_finite() || floor <= 0.0 {
        return Err("residual denominator floor must be finite and positive".into());
    }
    let e = stable_l2(error)?;
    let b = stable_l2(rhs)?;
    let relative = e / floor.max(b);
    if !relative.is_finite() {
        return Err("residual certification is nonfinite".into());
    }
    Ok((e, relative))
}



pub fn residual_certificate_columns(
    error: &[f64],
    rhs: &[f64],
    rows: usize,
    cols: usize,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    if error.len() != rhs.len() || error.len() != rows * cols {
        return Err("residual and RHS shapes differ".into());
    }
    let e = stable_l2_columns(error, rows, cols)?;
    let b = stable_l2_columns(rhs, rows, cols)?;
    let relative: Vec<f64> = e.iter().zip(&b).map(|(e, b)| e / 1.0_f64.max(*b)).collect();
    if relative.iter().any(|v| !v.is_finite()) {
        return Err("residual certification is nonfinite".into());
    }
    Ok((e, relative))
}

#[must_use]
pub fn norm2(x: &[f64]) -> f64 {
    x.iter().map(|v| v * v).sum::<f64>().sqrt()
}

