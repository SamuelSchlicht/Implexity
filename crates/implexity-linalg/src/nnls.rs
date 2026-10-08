// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub fn nonnegative_least_squares(a: &[Vec<f64>], b: &[f64]) -> Result<Vec<f64>, crate::LinalgError> {
    if a.len() != b.len()
        || a.is_empty()
        || a.iter().any(|r| r.len() != a[0].len() || r.iter().any(|v| !v.is_finite()))
        || b.iter().any(|v| !v.is_finite())
    {
        return Err(crate::LinalgError::Shape("nonnegative least-squares input shape or values".into()));
    }
    let n = a.first().map_or(0, Vec::len);
    let mut ata = vec![vec![0.0; n]; n];
    let mut atb = vec![0.0; n];
    for (row, bi) in a.iter().zip(b) {
        for i in 0..n {
            atb[i] += row[i] * bi;
            for j in 0..n {
                ata[i][j] += row[i] * row[j];
            }
        }
    }
    let mut x = vec![0.0; n];
    for _ in 0..20_000 {
        let mut change = 0.0_f64;
        for i in 0..n {
            if ata[i][i] <= 0.0 {
                continue;
            }
            let g: f64 = (0..n).map(|j| ata[i][j] * x[j]).sum::<f64>() - atb[i];
            let next = (x[i] - g / ata[i][i]).max(0.0);
            change = change.max((next - x[i]).abs());
            x[i] = next;
        }
        if change < 1e-14 {
            break;
        }
    }
    Ok(x)
}
