// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

