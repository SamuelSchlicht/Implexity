// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use crate::error::LinalgError;
use crate::sparse::CsrMatrix;

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}




pub fn connected_components(g: &CsrMatrix) -> Result<(usize, Vec<usize>), LinalgError> {
    let (n, m) = g.shape();
    if n != m {
        return Err(LinalgError::Shape(format!("graph adjacency must be square, got {n}×{m}")));
    }
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for &j in g.row(i).0 {
            let (a, b) = (find(&mut parent, i), find(&mut parent, j));
            if a != b {
                let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                parent[hi] = lo;
            }
        }
    }
    let mut label_of_root = vec![usize::MAX; n];
    let mut labels = vec![0usize; n];
    let mut count = 0;
    for (v, label) in labels.iter_mut().enumerate() {
        let r = find(&mut parent, v);
        if label_of_root[r] == usize::MAX {
            label_of_root[r] = count;
            count += 1;
        }
        *label = label_of_root[r];
    }
    Ok((count, labels))
}



pub fn connected_components_edges(
    n: usize,
    edges: &[(usize, usize)],
) -> Result<(usize, Vec<usize>), LinalgError> {
    let rows: Vec<usize> = edges.iter().map(|e| e.0).collect();
    let cols: Vec<usize> = edges.iter().map(|e| e.1).collect();
    let g = CsrMatrix::from_triplets(n, n, &rows, &cols, &vec![1.0; edges.len()])?;
    connected_components(&g)
}

