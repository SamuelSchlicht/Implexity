// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError, CaeResult};
#[derive(Clone, Copy)]
pub enum OriginBranch {
    Closed,
    Open,
    Symmetric,
}
pub fn residual_partials(
    gap: f64,
    multiplier: f64,
    gap_scale: f64,
    force_scale: f64,
    origin: OriginBranch,
) -> CaeResult<(f64, f64, f64)> {
    if ![gap, multiplier, gap_scale, force_scale]
        .iter()
        .all(|v| v.is_finite())
        || gap_scale <= 0.
        || force_scale <= 0.
    {
        return Err(CaeError::contract("invalid complementarity inputs"));
    }
    let a = gap / gap_scale;
    let b = multiplier / force_scale;
    let r = a.hypot(b);
    let (da, db) = if r == 0. {
        match origin {
            OriginBranch::Closed => (-1., 0.),
            OriginBranch::Open => (0., -1.),
            OriginBranch::Symmetric => (1. / 2f64.sqrt() - 1., 1. / 2f64.sqrt() - 1.),
        }
    } else {
        (a / r - 1., b / r - 1.)
    };
    let result = (r - a - b, da / gap_scale, db / force_scale);
    if ![a, b, r, result.0, result.1, result.2]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(CaeError::contract("contact normalized overflow"));
    }
    Ok(result)
}

pub fn origin_direction(
    matrix: &implexity_linalg::dense::DenseMatrix,
    residual: &[f64],
    gap_gradient: &[f64],
    gap_scale: f64,
    force_scale: f64,
) -> CaeResult<(Vec<f64>, implexity_linalg::dense::DenseMatrix, &'static str)> {
    use implexity_linalg::dense::DenseLu;
    let n = residual.len();
    if n < 2
        || matrix.nrows != n
        || matrix.ncols != n
        || gap_gradient.len() != n
        || gap_gradient[n - 1] != 0.
        || ![gap_scale, force_scale]
            .iter()
            .all(|x| x.is_finite() && *x > 0.)
        || residual
            .iter()
            .chain(gap_gradient)
            .chain(&matrix.data)
            .any(|x| !x.is_finite())
    {
        return Err(CaeError::contract("invalid origin predictor inputs"));
    }
    let rhs: Vec<_> = residual.iter().map(|x| -x).collect();
    for closed in [true, false] {
        let mut candidate = matrix.clone();
        for j in 0..n {
            candidate.data[(n - 1) * n + j] = if closed {
                -gap_gradient[j] / gap_scale
            } else if j == n - 1 {
                -1. / force_scale
            } else {
                0.
            };
        }
        let Ok(factor) = DenseLu::new(&candidate) else {
            continue;
        };
        let Ok(direction) = factor.solve(&rhs, 1, false) else {
            continue;
        };
        if direction.iter().any(|x| !x.is_finite()) {
            continue;
        }
        let predicted_gap: f64 = gap_gradient
            .iter()
            .zip(&direction)
            .map(|(g, d)| g * d)
            .sum();
        if !predicted_gap.is_finite() {
            continue;
        }

        if (closed && direction[n - 1] >= 0.) || (!closed && predicted_gap >= 0.) {
            return Ok((direction, candidate, if closed { "closed" } else { "open" }));
        }
    }
    Err(CaeError::contract(
        "no cone-consistent contact origin predictor",
    ))
}
