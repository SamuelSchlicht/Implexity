// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::CaeError;
use implexity_linalg::sparse::CsrMatrix;



pub fn cell_center_temperature_map(grid: &[i64], node_ijk: &[[i64; 3]]) -> Result<CsrMatrix, CaeError> {
    if grid.len() != 3 || grid.iter().any(|n| *n < 1) {
        return Err(CaeError::contract("temperature restriction requires a positive Cartesian cell grid"));
    }
    let shape: Vec<usize> = grid.iter().map(|n| usize::try_from(*n).unwrap_or(0)).collect();
    let nshape = [shape[0] + 1, shape[1] + 1, shape[2] + 1];
    let nnodes = nshape[0] * nshape[1] * nshape[2];
    let expected = (0..nnodes).all(|i| {
        let ijk = [i / (nshape[1] * nshape[2]), (i / nshape[2]) % nshape[1], i % nshape[2]];
        node_ijk.get(i).is_some_and(|p| (0..3).all(|k| usize::try_from(p[k]).ok() == Some(ijk[k])))
    });
    if node_ijk.len() != nnodes || !expected {
        return Err(CaeError::contract(
            "temperature restriction requires the declared C-order Cartesian node coordinates",
        ));
    }
    let ncells = shape[0] * shape[1] * shape[2];
    let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
    for c in 0..ncells {
        let cell = [c / (shape[1] * shape[2]), (c / shape[2]) % shape[1], c % shape[2]];
        for corner in 0..8 {
            let v = [cell[0] + (corner >> 2 & 1), cell[1] + (corner >> 1 & 1), cell[2] + (corner & 1)];
            rows.push(c);
            cols.push((v[0] * nshape[1] + v[1]) * nshape[2] + v[2]);
            vals.push(0.125);
        }
    }
    let q = CsrMatrix::from_triplets(ncells, nnodes, &rows, &cols, &vals)
        .map_err(|e| CaeError::contract(e.to_string()))?;
    let ones = q.matvec(&vec![1.0; nnodes]).map_err(|e| CaeError::contract(e.to_string()))?;
    let mut ok = ones.iter().all(|v| (*v - 1.0).abs() == 0.0);
    for k in 0..3 {
        let coord: Vec<f64> = node_ijk.iter().map(|p| p[k] as f64).collect();
        let centre = q.matvec(&coord).map_err(|e| CaeError::contract(e.to_string()))?;
        for (c, value) in centre.iter().enumerate() {
            let cell = [c / (shape[1] * shape[2]), (c / shape[2]) % shape[1], c % shape[2]];
            ok &= (*value - (cell[k] as f64 + 0.5)).abs() == 0.0;
        }
    }
    if !ok {
        return Err(CaeError::contract("temperature restriction failed its affine reproduction contract"));
    }
    Ok(q)
}
